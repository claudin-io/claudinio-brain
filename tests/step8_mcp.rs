//! Passo 8: the brain as an MCP server.
//!
//! These drive the real binary over a real stdio pipe rather than calling
//! `BrainServer` directly, because most of what can go wrong here is not in the
//! tool bodies. It is in the wiring: a schema the SDK rejects at startup, a
//! diagnostic printed to stdout, an error mapped to the wrong code. None of that
//! is reachable from an in-process handler call.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    root: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("xdg/config")).unwrap();
        std::fs::create_dir_all(root.join("xdg/data")).unwrap();
        let s = Self { _tmp: tmp, root };
        s.cli(&["init", "--label", "loja"]);
        s
    }

    fn bin(&self) -> Command {
        let mut c = Command::new(assert_cmd::cargo::cargo_bin("brain"));
        c.current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BRAIN_CONFIG_DIR", self.root.join("xdg/config"))
            .env("BRAIN_DATA_DIR", self.root.join("xdg/data"));
        c
    }

    fn cli(&self, args: &[&str]) {
        let out = self.bin().args(args).output().unwrap();
        assert!(
            out.status.success(),
            "`brain {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Runs one MCP session: performs the handshake, sends `calls`, and returns
    /// every response keyed by request id.
    ///
    /// stdin is closed after the last request, which is how the server is asked
    /// to shut down -- an MCP server over stdio ends when its client goes away.
    fn mcp(&self, calls: &[(i64, &str, serde_json::Value)]) -> Session {
        let mut child = self
            .bin()
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        {
            let stdin = child.stdin.as_mut().unwrap();
            let hello = serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            });
            writeln!(stdin, "{hello}").unwrap();
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .unwrap();
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
            )
            .unwrap();

            for (id, name, args) in calls {
                let req = serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": "tools/call",
                    "params": {"name": name, "arguments": args}
                });
                writeln!(stdin, "{req}").unwrap();
            }
        }
        // Dropping stdin closes the pipe and ends the session.
        drop(child.stdin.take());

        let stdout = child.stdout.take().unwrap();
        let mut lines = Vec::new();
        let mut by_id = std::collections::BTreeMap::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.trim().is_empty() {
                continue;
            }
            // Every non-empty byte on stdout must be a JSON-RPC frame. This is
            // the assertion that catches a stray `println!` or `dbg!` anywhere
            // in the crate, which would corrupt the stream and disconnect a real
            // client with no diagnostic at all.
            let v: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("non-JSON on stdout: {line:?} ({e})"));
            assert_eq!(v["jsonrpc"], "2.0", "not a JSON-RPC frame: {line}");
            if let Some(id) = v["id"].as_i64() {
                by_id.insert(id, v.clone());
            }
            lines.push(v);
        }

        let out = child.wait_with_output().unwrap();
        Session {
            by_id,
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

struct Session {
    by_id: std::collections::BTreeMap<i64, serde_json::Value>,
    stderr: String,
}

impl Session {
    fn result(&self, id: i64) -> &serde_json::Value {
        let v = self
            .by_id
            .get(&id)
            .unwrap_or_else(|| panic!("no response for id {id}; stderr:\n{}", self.stderr));
        assert!(
            v.get("error").is_none(),
            "id {id} returned an error: {}",
            v["error"]
        );
        &v["result"]
    }

    /// The `structuredContent` of a tool call.
    fn content(&self, id: i64) -> &serde_json::Value {
        &self.result(id)["structuredContent"]
    }

    fn error(&self, id: i64) -> &serde_json::Value {
        let v = self.by_id.get(&id).expect("a response");
        v.get("error")
            .unwrap_or_else(|| panic!("id {id} succeeded, expected an error: {v}"))
    }
}

#[test]
fn the_server_identifies_itself_and_the_brain_it_is_bound_to() {
    let s = Sandbox::new();
    let session = s.mcp(&[]);

    let info = session.result(1);
    assert_eq!(info["serverInfo"]["name"], "claudinio-brain");

    // An agent holding several memory servers has to be able to tell which brain
    // this one speaks for. Since the binding is fixed for the session, it is
    // stated once here rather than stamped on every response.
    let instructions = info["instructions"].as_str().expect("instructions");
    assert!(
        instructions.contains("loja"),
        "instructions do not name the brain: {instructions}"
    );
}

#[test]
fn every_tool_is_listed_with_a_schema() {
    let s = Sandbox::new();
    let session = s.mcp(&[]);
    let tools = session.result(2)["tools"].as_array().unwrap().clone();

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "remember", "link", "recall", "get", "history", "entity", "why", "retract", "alias",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}: {names:?}"
        );
    }

    for t in &tools {
        let name = t["name"].as_str().unwrap();
        // A description is not decoration: it is the only thing an agent reads
        // before deciding whether to call.
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 40),
            "{name} has no usable description"
        );
        // MCP requires the root of both schemas to be an object. rmcp panics at
        // startup when an output schema violates that, which is how it was found.
        assert_eq!(t["inputSchema"]["type"], "object", "{name} input schema");
        assert_eq!(t["outputSchema"]["type"], "object", "{name} output schema");
    }
}

#[test]
fn the_bitemporal_promise_survives_the_protocol() {
    let s = Sandbox::new();
    s.cli(&[
        "remember",
        "--subject",
        "produto_a",
        "--predicate",
        "preco",
        "--value",
        "20",
        "--at",
        "2026-01-01",
    ]);
    s.cli(&[
        "remember",
        "--subject",
        "produto_a",
        "--predicate",
        "preco",
        "--value",
        "25",
        "--at",
        "2026-06-01",
    ]);

    let session = s.mcp(&[
        (
            10,
            "get",
            serde_json::json!({"subject": "produto_a", "predicate": "preco"}),
        ),
        (
            11,
            "get",
            serde_json::json!({"subject": "produto_a", "predicate": "preco", "as_of": "2026-03-01"}),
        ),
        (
            12,
            "history",
            serde_json::json!({"subject": "produto_a", "predicate": "preco"}),
        ),
    ]);

    assert_eq!(session.content(10)["fact"]["object_num"], 25.0);
    assert_eq!(session.content(11)["fact"]["object_num"], 20.0);
    assert_eq!(session.content(12)["facts"].as_array().unwrap().len(), 2);
}

#[test]
fn a_relation_written_over_mcp_is_walkable() {
    let s = Sandbox::new();
    let session = s.mcp(&[
        (
            10,
            "link",
            serde_json::json!({"from": "produto_a", "rel": "fornecido_por", "to": "acme"}),
        ),
        (
            11,
            "remember",
            serde_json::json!({"subject": "acme", "predicate": "pais", "value": "Chile"}),
        ),
        (
            12,
            "recall",
            serde_json::json!({"query": "de que pais vem o produto_a"}),
        ),
    ]);

    assert_eq!(session.content(10)["outcome"], "created");
    // The answer shares no word with the question and is one hop past the entity
    // the question names. Getting it first is the whole reason for the graph.
    let hits = session.content(12)["hits"].as_array().unwrap();
    assert_eq!(hits[0]["fact"]["object_text"], "Chile");
}

#[test]
fn recall_does_not_learn_unless_asked() {
    let s = Sandbox::new();
    for (subject, value) in [("Produto Brasília", "20"), ("servidor", "8080")] {
        s.cli(&[
            "remember",
            "--subject",
            subject,
            "--predicate",
            if subject == "servidor" {
                "porta"
            } else {
                "preco"
            },
            "--value",
            value,
        ]);
    }
    s.cli(&[
        "remember",
        "--subject",
        "cache",
        "--predicate",
        "ttl",
        "--value",
        "300",
    ]);

    let q = "quanto custa o produto brasilia";
    let session = s.mcp(&[
        (10, "recall", serde_json::json!({"query": q})),
        (
            11,
            "entity",
            serde_json::json!({"name": "Produto Brasília"}),
        ),
        (12, "recall", serde_json::json!({"query": q, "learn": true})),
        (
            13,
            "entity",
            serde_json::json!({"name": "Produto Brasília"}),
        ),
    ]);

    // A read that writes is a read that cannot be replayed, so the default must
    // leave nothing behind.
    assert!(session.content(10)["learned"].is_null());
    assert_eq!(
        session.content(11)["entity"]["aliases"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    assert_eq!(session.content(12)["learned"]["alias"], "produto_brasilia");
    let names = session.content(13)["entity"]["aliases"].as_array().unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0]["source"], "learned");
}

#[test]
fn a_missing_thing_is_reported_as_missing_rather_than_broken() {
    let s = Sandbox::new();
    let session = s.mcp(&[
        (10, "why", serde_json::json!({"fact_id": 99})),
        (
            11,
            "alias",
            serde_json::json!({"entity": "fantasma", "alias": "apelido"}),
        ),
        (
            12,
            "remember",
            serde_json::json!({"subject": "produto_a", "predicate": "preco"}),
        ),
    ]);

    // -32002 is RESOURCE_NOT_FOUND and -32602 INVALID_PARAMS. An agent can act on
    // either -- ask for something else, or fix the call. An internal error (-32603)
    // tells it only to give up, so mapping these correctly is what decides whether
    // the agent recovers.
    assert_eq!(session.error(10)["code"], -32002);
    assert_eq!(session.error(11)["code"], -32002);
    assert_eq!(session.error(12)["code"], -32602);
}
