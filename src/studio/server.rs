//! The studio's write side: a localhost HTTP server, hand-rolled.
//!
//! Deliberately not a web framework. The surface is one page and seven JSON
//! endpoints on a loopback socket, and pulling in an async runtime plus a router
//! to serve that would cost more than it explains. The whole protocol here is
//! request line, headers, `Content-Length` body, response -- and `Connection:
//! close` on every reply, so there is no keep-alive state machine to get wrong.
//!
//! # Why a loopback bind is not, by itself, enough
//!
//! Binding `127.0.0.1` keeps other *machines* out. It does not keep out the page
//! the user has open in another tab: a browser will happily let any origin POST
//! to `http://127.0.0.1:9999`, and while CORS stops that origin from *reading*
//! the reply, a write does not need to be read to have happened. Two things
//! close that:
//!
//! - **A token in a custom header.** `X-Brain-Token` is not a CORS-safelisted
//!   header, so sending it forces a preflight, and the preflight fails because
//!   nothing here answers `OPTIONS` with permission. A cross-origin write cannot
//!   get off the ground.
//! - **A `Host` allowlist.** A hostname that resolves to 127.0.0.1 -- the DNS
//!   rebinding trick -- makes the request same-origin from the browser's point of
//!   view, and the token would travel with it. Checking that `Host` is literally
//!   loopback is what makes the token check meaningful.
//!
//! Neither is theatre: without the first, any visited page can rewrite the
//! brain; without the second, the first is bypassable.

use crate::brain::{Assertion, Brain, Object, Outcome};
use crate::recall::{RecallQuery, When};
use crate::studio::{Snapshot, render_page};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Enough for any browser's headers, small enough that a hostile client cannot
/// make us buffer forever.
const MAX_HEAD: usize = 32 * 1024;
/// A fact is text. Anything larger than this is not one.
const MAX_BODY: usize = 4 * 1024 * 1024;

const READ_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Studio {
    brain: Brain,
    listener: TcpListener,
    token: String,
    port: u16,
}

impl Studio {
    /// Binds the loopback interface. Port 0 asks the OS for a free one.
    pub fn bind(brain: Brain, port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            brain,
            listener,
            token: new_token(),
            port,
        })
    }

    /// The URL to open. Carries the token, so it is a capability -- and belongs
    /// in a terminal rather than anywhere it would be logged.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/?t={}", self.port, self.token)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Serves until the process is stopped.
    ///
    /// One connection at a time. A single user driving one page cannot outrun
    /// SQLite, and the alternative -- sharing a `Brain` across threads -- would
    /// mean a mutex around every read for no gain the user could perceive. The
    /// read timeout is what keeps a browser's speculative preconnect from
    /// holding the loop.
    pub fn run(&self) -> std::io::Result<()> {
        for incoming in self.listener.incoming() {
            match incoming {
                Ok(mut stream) => {
                    if let Err(e) = self.serve_one(&mut stream) {
                        tracing::debug!(error = %e, "studio: connection ended");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "studio: accept failed"),
            }
        }
        Ok(())
    }

    fn serve_one(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

        let req = match read_request(stream)? {
            Some(r) => r,
            None => return Ok(()), // connection opened and closed without asking
        };

        let reply = self.route(&req);
        write_response(stream, &reply)
    }

    fn route(&self, req: &Request) -> Reply {
        // Rebinding check first: every other check below is only as good as the
        // guarantee that this request really came from loopback.
        if !self.host_is_loopback(req.header("host")) {
            return Reply::text(403, "forbidden: Host is not loopback");
        }

        let authorized = match req.path.as_str() {
            // The page itself is opened from a pasted URL, so its token arrives
            // in the query string. Every API call is made by that page, which
            // can put it where a cross-origin caller cannot.
            "/" => constant_time_eq(req.query_param("t").as_deref().unwrap_or(""), &self.token),
            _ => constant_time_eq(req.header("x-brain-token").unwrap_or(""), &self.token),
        };
        if !authorized {
            return Reply::text(403, "forbidden: bad or missing studio token");
        }

        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => self.page(),
            ("GET", "/api/snapshot") => self.json_result(self.snapshot_value()),
            ("POST", "/api/remember") => self.json_result(self.remember(req)),
            ("POST", "/api/link") => self.json_result(self.link(req)),
            ("POST", "/api/retract") => self.json_result(self.retract(req)),
            ("POST", "/api/alias") => self.json_result(self.alias(req)),
            ("POST", "/api/recall") => self.json_result(self.recall(req)),
            ("POST", "/api/why") => self.json_result(self.why(req)),
            _ => Reply::text(404, "not found"),
        }
    }

    // --- routes --------------------------------------------------------------

    fn page(&self) -> Reply {
        match Snapshot::capture(&self.brain, true).map_err(|e| e.to_string()) {
            Ok(snap) => match render_page(&snap) {
                Ok(html) => Reply::html(html),
                Err(e) => Reply::text(500, &format!("render failed: {e}")),
            },
            Err(e) => Reply::text(500, &format!("snapshot failed: {e}")),
        }
    }

    fn snapshot_value(&self) -> Result<Value, String> {
        let snap = Snapshot::capture(&self.brain, true).map_err(|e| e.to_string())?;
        serde_json::to_value(snap).map_err(|e| e.to_string())
    }

    /// Every write answers with the fresh snapshot as well as the outcome.
    ///
    /// One round trip, and the page never has to reproduce the brain's placement
    /// rules to guess what its own edit did. Whether a write superseded, or
    /// corrected, or merely reinforced is decided in the core; a client that
    /// re-derived it would eventually disagree with it.
    fn with_snapshot(&self, outcome: Value) -> Result<Value, String> {
        Ok(json!({ "outcome": outcome, "snapshot": self.snapshot_value()? }))
    }

    fn remember(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let subject = string_field(&b, "subject")?;
        let predicate = string_field(&b, "predicate")?;

        let object = match (b.get("entity").and_then(Value::as_str), b.get("value")) {
            (Some(e), _) => Object::entity(e),
            (None, Some(Value::Number(n))) => {
                let o = Object::num(n.as_f64().unwrap_or_default());
                match b.get("unit").and_then(Value::as_str) {
                    Some(u) => o.with_unit(u),
                    None => o,
                }
            }
            // The CLI reads a bare `--value 10` as the number ten; the studio's
            // form posts strings, so it has to make the same call here or the
            // same input would land as text from one surface and a number from
            // the other.
            (None, Some(Value::String(s))) => match s.parse::<f64>() {
                Ok(n) => {
                    let o = Object::num(n);
                    match b.get("unit").and_then(Value::as_str) {
                        Some(u) => o.with_unit(u),
                        None => o,
                    }
                }
                Err(_) => Object::text(s.clone()),
            },
            _ => return Err("pass `value` or `entity`".into()),
        };

        let mut a = Assertion::new(subject, predicate, object);
        a.valid_from = when_field(&b, "at")?;
        a.source = b.get("source").and_then(Value::as_str).map(str::to_string);
        a.scope = b.get("scope").and_then(Value::as_str).map(str::to_string);
        a.confidence = b.get("confidence").and_then(Value::as_f64);
        a.locator = b.get("locator").cloned().filter(|v| !v.is_null());
        a.cardinality = b
            .get("cardinality")
            .and_then(Value::as_str)
            .and_then(crate::brain::Cardinality::parse);

        let outcome = self.brain.remember(&a).map_err(|e| e.to_string())?;
        self.with_snapshot(outcome_json(&outcome))
    }

    fn link(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let outcome = self
            .brain
            .link(
                &string_field(&b, "from")?,
                &string_field(&b, "rel")?,
                &string_field(&b, "to")?,
                when_field(&b, "at")?,
            )
            .map_err(|e| e.to_string())?;
        self.with_snapshot(outcome_json(&outcome))
    }

    fn retract(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let id = b
            .get("fact_id")
            .and_then(Value::as_i64)
            .ok_or("`fact_id` is required")?;
        let fact = self
            .brain
            .retract(id, b.get("reason").and_then(Value::as_str))
            .map_err(|e| e.to_string())?;
        self.with_snapshot(json!({ "kind": "retracted", "fact": fact }))
    }

    fn alias(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let entity = string_field(&b, "entity")?;
        let alias = string_field(&b, "alias")?;

        if b.get("forget").and_then(Value::as_bool).unwrap_or(false) {
            let removed = self
                .brain
                .forget_alias(&entity, &alias)
                .map_err(|e| e.to_string())?;
            return self.with_snapshot(json!({ "kind": "alias_forgotten", "removed": removed }));
        }
        let a = self
            .brain
            .declare_alias(&entity, &alias)
            .map_err(|e| e.to_string())?;
        self.with_snapshot(json!({ "kind": "alias_declared", "alias": a }))
    }

    /// Recall, with the per-channel attribution left on.
    ///
    /// This is the endpoint the studio's trace panel is built on: `Hit.channels`
    /// is what turns "why did that rank first" from a guess into a reading.
    fn recall(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let mut q = RecallQuery::new(string_field(&b, "query")?);
        q.limit = b
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(200) as usize;
        q.scope = b.get("scope").and_then(Value::as_str).map(str::to_string);
        q.when = if b.get("history").and_then(Value::as_bool).unwrap_or(false) {
            When::History
        } else {
            match when_field(&b, "as_of")? {
                Some(t) => When::AsOf(t),
                None => When::Now,
            }
        };

        let hits = self.brain.recall(&q).map_err(|e| e.to_string())?;
        Ok(json!({ "hits": hits }))
    }

    fn why(&self, req: &Request) -> Result<Value, String> {
        let b = req.json()?;
        let id = b
            .get("fact_id")
            .and_then(Value::as_i64)
            .ok_or("`fact_id` is required")?;
        let p = self.brain.why(id).map_err(|e| e.to_string())?;
        serde_json::to_value(p).map_err(|e| e.to_string())
    }

    // --- plumbing ------------------------------------------------------------

    fn json_result(&self, r: Result<Value, String>) -> Reply {
        match r {
            Ok(v) => Reply::json(200, &v),
            // 400 rather than 500: everything reaching here is a rejected
            // assertion or a bad field, which is the caller's to fix.
            Err(e) => Reply::json(400, &json!({ "error": e })),
        }
    }

    fn host_is_loopback(&self, host: Option<&str>) -> bool {
        let Some(host) = host else { return false };
        // A port-less Host cannot happen for a non-default port, but accepting
        // the bare forms costs nothing and keeps curl usable.
        matches!(
            host,
            h if h == format!("127.0.0.1:{}", self.port)
                || h == format!("localhost:{}", self.port)
                || h == format!("[::1]:{}", self.port)
                || h == "127.0.0.1"
                || h == "localhost"
        )
    }
}

fn outcome_json(o: &Outcome) -> Value {
    let mut v = json!({ "kind": o.kind(), "fact": o.fact() });
    match o {
        // What a write *closed* is the part a graph has to redraw, and the part
        // a person is most likely to have not expected.
        Outcome::Superseded { closed, .. } => v["closed"] = json!(closed),
        Outcome::Corrected { retracted, .. } => v["retracted"] = json!(retracted),
        Outcome::Created(_) | Outcome::Reasserted(_) => {}
    }
    v
}

fn string_field(b: &Value, key: &str) -> Result<String, String> {
    b.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("`{key}` is required"))
}

fn when_field(b: &Value, key: &str) -> Result<Option<jiff::Timestamp>, String> {
    match b.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => crate::cli::parse_when(s)
            .map(Some)
            .map_err(|e| e.to_string()),
    }
}

/// 128 random bits, hex. Not derived from the port or the clock: a token anyone
/// can predict is a token that is not there.
fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Compares without leaking where the mismatch was, in time.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    // The length is not a secret -- the token's length is fixed and public --
    // but the position of the first differing byte is.
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

// --- HTTP --------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn query_param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (percent_decode(k) == name).then(|| percent_decode(v))
        })
    }

    fn json(&self) -> Result<Value, String> {
        if self.body.is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_slice(&self.body).map_err(|e| format!("body is not JSON: {e}"))
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut head = Vec::new();
    loop {
        let before = head.len();
        let budget = (MAX_HEAD - before) as u64;
        let n = reader.by_ref().take(budget).read_until(b'\n', &mut head)?;
        if n == 0 {
            // Clean close before a complete request: a preconnect, usually.
            return Ok(None);
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
        if head.len() >= MAX_HEAD {
            return Err(std::io::Error::other("request headers too large"));
        }
    }

    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let start = lines.next().unwrap_or_default();
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }

    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(std::io::Error::other("request body too large"));
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        path: percent_decode(&path),
        query,
        headers,
        body,
    }))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn html(body: String) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
        }
    }

    fn json(status: u16, v: &Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.as_bytes().to_vec(),
        }
    }
}

fn write_response(stream: &mut TcpStream, r: &Reply) -> std::io::Result<()> {
    let reason = match r.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    // No `Access-Control-Allow-*` anywhere, on purpose: the absence is what makes
    // the preflight for `X-Brain-Token` fail, and that failure is the CSRF
    // defence described at the top of this module.
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {ct}\r\n\
         Content-Length: {len}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n",
        status = r.status,
        ct = r.content_type,
        len = r.body.len(),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

/// Best effort. A studio that refuses to start because the desktop has no
/// browser helper would be worse than one that just prints the URL.
pub fn open_in_browser(url: &str) {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        ("cmd", &["/C", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let spawned = std::process::Command::new(cmd)
        .args(args)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "studio: could not open a browser; use the URL above");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_str_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        // A truncated escape is data, not a parse error: the studio must not
        // fall over on a malformed URL.
        assert_eq!(percent_decode("100%"), "100%");
    }
}
