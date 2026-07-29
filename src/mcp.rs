//! The MCP server: the brain as a tool surface an agent can call.
//!
//! Everything here is a thin shell over the same library the CLI uses. No
//! retrieval logic lives in this file, and none should: a tool that answered
//! differently from `brain recall` would make the CLI useless for debugging what
//! an agent saw.
//!
//! Three things are worth knowing before reading further.
//!
//! **stdout is the transport.** In this mode stdout carries JSON-RPC frames, so
//! a single stray `println!` corrupts the stream and the client disconnects with
//! no diagnostic. `clippy::print_stdout` is denied crate-wide for this reason;
//! diagnostics go to stderr through `tracing`.
//!
//! **One server, one brain.** The brain is resolved once at startup through the
//! same eight-rung ladder the CLI uses, and never changes for the life of the
//! process. This is why tool responses are *not* individually stamped with
//! `brain_id`, unlike every CLI answer: there, each invocation could name a
//! different file, so the stamp is the only thing preventing an agent from
//! attributing one brain's facts to another. Here the binding is fixed at
//! initialize, so the identity is stated once in the server instructions instead
//! of repeated on every call.
//!
//! **Tools are synchronous.** SQLite serializes writes anyway and every
//! operation here is sub-millisecond, so the brain sits behind a plain `Mutex`
//! and no lock is ever held across an await. Making these `async` would buy
//! nothing and add a way to deadlock.

use crate::brain::{Assertion, Brain, BrainError, Object, Order, WhichQuery};
use crate::recall::{RecallQuery, When};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ErrorData, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, ServiceExt, schemars, tool, tool_router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Mutex;

pub struct BrainServer {
    brain: Mutex<Brain>,
    label: String,
    id: String,
    path: String,
}

/// Serves one brain over stdio until the client disconnects.
pub fn serve(brain: Brain) -> anyhow::Result<()> {
    let server = BrainServer::new(brain);

    // A single-threaded runtime: the work is one mutex-guarded SQLite
    // connection, so extra worker threads would only contend for it.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let service = server.serve(rmcp::transport::stdio()).await?;
            service.waiting().await?;
            Ok::<(), anyhow::Error>(())
        })
}

impl BrainServer {
    pub fn new(brain: Brain) -> Self {
        let (label, id, path) = {
            let store = brain.store();
            (
                store.label().to_string(),
                store.id().to_string(),
                store.path().display().to_string(),
            )
        };
        Self {
            brain: Mutex::new(brain),
            label,
            id,
            path,
        }
    }

    /// Runs `f` against the brain, mapping any failure into an MCP error.
    ///
    /// A poisoned mutex means a previous tool panicked while holding the brain.
    /// That is reported rather than propagated by panicking again, because the
    /// panic already went to stderr and a second one would take the transport
    /// down with it.
    fn with<T>(&self, f: impl FnOnce(&Brain) -> Result<T, BrainError>) -> Result<T, ErrorData> {
        let guard = self.brain.lock().map_err(|_| {
            ErrorData::internal_error("brain lock poisoned by an earlier panic", None)
        })?;
        f(&guard).map_err(to_mcp_error)
    }
}

/// Distinguishes "you asked for something that is not there" from "the brain
/// broke", because an agent can act on the first and can only retry the second.
fn to_mcp_error(e: BrainError) -> ErrorData {
    match &e {
        BrainError::NoSuchFact(_) | BrainError::NoSuchEntity { .. } => {
            ErrorData::resource_not_found(e.to_string(), None)
        }
        BrainError::EmptyKey { .. }
        | BrainError::InvalidConfidence(_)
        | BrainError::AliasTaken { .. }
        | BrainError::CardinalityConflict { .. } => ErrorData::invalid_params(e.to_string(), None),
        _ => ErrorData::internal_error(e.to_string(), None),
    }
}

fn parse_at(s: Option<&String>) -> Result<Option<jiff::Timestamp>, ErrorData> {
    s.map(|w| crate::cli::parse_when(w))
        .transpose()
        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))
}

// --- parameters ---------------------------------------------------------------
//
// The doc comment on each field becomes its description in the tool's JSON
// schema, which is the only thing an agent reads before calling. They are
// written for that audience.

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RememberParams {
    /// The thing the fact is about, e.g. `produto_a`, `api_gateway`, `deploy`.
    pub subject: String,
    /// The property being recorded, e.g. `preco`, `timeout`, `responsavel`.
    pub predicate: String,
    /// **Use this whenever the value names something you could ask another
    /// question about** -- a class, an owner, a supplier, a service, a category.
    /// It records a relation, which is the only kind of fact the graph can be
    /// walked through, and it creates the named entity if it does not exist yet.
    ///
    /// `is_a`, `depends_on`, `owned_by`, `part_of` and anything like them always
    /// belong here. Passing `voucher_sazonal` as `value` instead stores a string
    /// that no walk can follow: the fact reads back correctly and is invisible
    /// to retrieval, and nothing will tell you.
    #[serde(default)]
    pub entity: Option<String>,
    /// A literal: a number, a date, a flag, a short piece of prose. A string
    /// that parses as a number is stored as one.
    ///
    /// Not for anything that names a thing -- use `entity` for that. Provide
    /// this or `entity`, never both.
    #[serde(default)]
    pub value: Option<String>,
    /// Unit of a numeric value, e.g. `s`, `MB`, `BRL`.
    #[serde(default)]
    pub unit: Option<String>,
    /// When this became true in the world, as `2026-01-01` or RFC 3339.
    /// Defaults to now. Set it whenever you learn something late, so it slots
    /// into the timeline instead of looking like it just changed.
    #[serde(default)]
    pub at: Option<String>,
    /// When this stops being true, if you already know. The fact closes itself
    /// then, instead of waiting for someone to supersede it.
    ///
    /// This is what makes a short-lived claim safe to record: a freeze that lifts
    /// on Friday, a token that expires in an hour, a state you will not be around
    /// to correct. Reasserting with a later `until` pushes the end back, so a fact
    /// can be kept alive by repetition. Without it, anything short-lived either
    /// rots into a false answer or should not be written at all.
    #[serde(default)]
    pub until: Option<String>,
    /// Who or what is asserting this. Pass it; attribution is what makes a fact
    /// reviewable later.
    #[serde(default)]
    pub source: Option<String>,
    /// JSON object saying where the answer really lives, e.g.
    /// `{"file":"src/pricing.rs","lines":"40-52"}`. Prefer this over pasting
    /// content into the value: a brain is an index, not a warehouse.
    #[serde(default)]
    pub locator: Option<serde_json::Value>,
    /// 0.0 to 1.0. Omit unless you genuinely have a reason to doubt it.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Optional namespace, to keep unrelated sets of facts apart.
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallParams {
    /// The question, in whatever words you have.
    pub query: String,
    /// Answer as of this instant instead of with what currently holds.
    #[serde(default)]
    pub as_of: Option<String>,
    /// Return the whole trajectory, closed intervals included, instead of only
    /// what is true now.
    #[serde(default)]
    pub history: bool,
    /// Maximum hits. Defaults to 10.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub scope: Option<String>,
    /// Let the brain keep the name this question used for the thing it found,
    /// if the answer was unambiguous. Off by default. Turn it on only when the
    /// question came from a person, never when it came from your own paraphrase
    /// of one -- otherwise the brain learns your wording rather than theirs.
    #[serde(default)]
    pub learn: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WhichParams {
    /// The property to list by, e.g. `status`, `owner`, `due`, `preco`.
    pub predicate: String,
    /// Match only subjects whose value is exactly this. Omit to list every
    /// subject that holds the predicate at all, whatever its value.
    #[serde(default)]
    pub value: Option<String>,
    /// Match an entity-valued object by identity rather than by spelling, so
    /// aliases and other spellings of the same thing are included. Provide this
    /// or `value`, never both.
    #[serde(default)]
    pub entity: Option<String>,
    /// List the set as it stood at this instant instead of as it stands now.
    #[serde(default)]
    pub as_of: Option<String>,
    /// Include closed intervals, i.e. subjects that used to match.
    #[serde(default)]
    pub history: bool,
    /// `subject` (default), `value` or `since`. Sort by `value` to order by an
    /// ISO-8601 date, which sorts chronologically.
    #[serde(default)]
    pub order_by: Option<String>,
    #[serde(default)]
    pub desc: bool,
    /// Defaults to 200. Compare `matched` against the number of facts returned
    /// to find out whether this cut anything off.
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetParams {
    pub subject: String,
    pub predicate: String,
    /// Read the value as it stood at this instant instead of the current one.
    #[serde(default)]
    pub as_of: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PairParams {
    pub subject: String,
    pub predicate: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LinkParams {
    pub from: String,
    /// The relation, e.g. `fornecido_por`, `depends_on`, `owned_by`.
    pub rel: String,
    pub to: String,
    /// When the relation started holding. Defaults to now.
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntityParams {
    pub name: String,
    /// Also walk the relations out from it and report what is reachable.
    #[serde(default)]
    pub neighbors: bool,
    /// Show the neighbourhood as it stood at this instant.
    #[serde(default)]
    pub as_of: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FactIdParams {
    /// The `id` field of a fact, as returned by any other tool.
    pub fact_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RetractParams {
    pub fact_id: i64,
    /// Why it was never true. Worth writing: this is what a later reader sees.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AliasParams {
    /// The entity that should answer to another name. Must already exist.
    pub entity: String,
    /// The other name, exactly as a person would write it.
    pub alias: String,
}

// --- results ------------------------------------------------------------------
//
// MCP requires a tool's output schema to have a root `type: object`, so a bare
// `Json<serde_json::Value>` is rejected at startup -- the panic is loud and
// immediate, which is the right behaviour for a contract violation.
//
// Each result is therefore a named struct whose fields are documented. The
// payloads inside stay `serde_json::Value` rather than the core types: deriving
// `JsonSchema` on `Fact` and everything it holds would mean pulling schemars
// through `jiff` and `uuid`, and putting a schema derive on the bitemporal core
// to satisfy an optional feature is the tail wagging the dog. The trade is that
// a fact's inner shape is not schema'd; it is self-describing JSON, and every
// field it can carry is documented in `brain/types.rs`.

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct WriteResult {
    /// What the write did: `created`, `superseded` (it changed), `corrected`
    /// (the old claim was never true) or `reasserted` (already known).
    pub outcome: String,
    /// The fact as stored, including the `id` other tools take.
    pub fact: serde_json::Value,
    /// Set when the write succeeded but probably did not mean what you wanted:
    /// most often a relation recorded as a string, which stores fine and is
    /// invisible to every walk of the graph. Null in the normal case. Read it --
    /// nothing else will ever raise this, because nothing failed.
    pub hint: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct RecallResult {
    /// Best first. Each hit carries the fact, its fused score, and which
    /// channels found it.
    pub hits: serde_json::Value,
    /// The name this question taught the brain, if `learn` was set and the
    /// answer was unambiguous. Null otherwise, which is the normal case.
    pub learned: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct WhichResult {
    /// The matching facts, in the order asked for.
    pub facts: serde_json::Value,
    /// How many matched in total, before `limit`. This is the number `recall`
    /// cannot give you: if it equals the length of `facts`, you are holding the
    /// complete set and can act on it as one.
    pub matched: i64,
    /// True when `limit` cut the answer short, so what you have is a prefix.
    pub truncated: bool,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ValueResult {
    /// The single answer, or null when nothing is known -- which is a real
    /// answer, and better than guessing.
    pub fact: serde_json::Value,
    /// Every open value. More than one only for a multi-valued predicate.
    pub facts: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct FactsResult {
    /// Oldest first.
    pub facts: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct EntityResult {
    /// The entity, its other names, its facts and (if asked) its neighbours.
    /// Null when no entity goes by that name.
    pub entity: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct ProvenanceResult {
    pub fact: serde_json::Value,
    /// The fact that replaced this one, if any.
    pub superseded_by: serde_json::Value,
    /// The fact this one replaced, if any.
    pub supersedes: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct RetractResult {
    /// The fact, now marked as never having been true.
    pub retracted: serde_json::Value,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub struct AliasResult {
    pub entity: String,
    /// The name as normalized, its source (`declared`) and its weight.
    pub alias: serde_json::Value,
}

// --- tools --------------------------------------------------------------------

// Not `#[tool_router(server_handler)]`: that emits the `ServerHandler` impl for
// us, and this server needs its own `get_info` to name the brain it is bound to.
#[tool_router]
impl BrainServer {
    /// Record a fact. Writing a new value does not overwrite the old one, it
    /// closes it, so the previous value stays answerable through `get` with
    /// `as_of` and through `history`.
    ///
    /// The response says which of four things happened, and they mean different
    /// things: `created` (new), `superseded` (it changed, the old value was true
    /// until now), `corrected` (a claim for the same instant was wrong and has
    /// been retracted), `reasserted` (already known, reinforced rather than
    /// duplicated). A `superseded` you did not expect means the brain already
    /// knew something -- check `history` before assuming your value is right.
    ///
    /// Record what is stable, specific and worth more than one session: a
    /// decision and its reason, a config value, an owner, a deadline, a
    /// constraint someone stated. Do not record what the repository or git
    /// history already says, transient state, or anything nobody actually
    /// asserted. A guess written as a fact is worse than no memory at all,
    /// because the next session will trust it.
    #[tool(name = "remember")]
    fn remember(
        &self,
        Parameters(p): Parameters<RememberParams>,
    ) -> Result<Json<WriteResult>, ErrorData> {
        let object = match (&p.value, &p.entity) {
            (_, Some(e)) => Object::entity(e),
            (Some(v), None) => match &p.unit {
                Some(u) => Object::parse_literal(v).with_unit(u),
                None => Object::parse_literal(v),
            },
            (None, None) => {
                return Err(ErrorData::invalid_params("pass `value` or `entity`", None));
            }
        };

        let mut a = Assertion::new(&p.subject, &p.predicate, object);
        a.valid_from = parse_at(p.at.as_ref())?;
        a.valid_to = parse_at(p.until.as_ref())?;
        a.source = p.source.clone();
        a.locator = p.locator.clone();
        a.confidence = p.confidence;
        a.scope = p.scope.clone();

        // The hint is looked up only for a text-valued write, and only after it
        // landed: the write is not in doubt, and a warning that could change the
        // outcome would make this a validator rather than a memory.
        let text_valued = p.entity.is_none();
        let (outcome, hint) = self.with(|b| {
            let outcome = b.remember(&a)?;
            let hint = if text_valued {
                crate::lint::missed_relation(b.store().conn(), &crate::norm::key(&p.predicate))?
            } else {
                None
            };
            Ok((outcome, hint))
        })?;

        Ok(Json(WriteResult {
            outcome: outcome.kind().to_string(),
            fact: json!(outcome.fact()),
            hint,
        }))
    }

    /// Record a relation between two entities. A relation is itself a fact, so
    /// it has a timeline too: a supplier that changed shows up as a closed
    /// interval rather than a deleted row.
    ///
    /// Relations are what let a question be answered by something stored nowhere
    /// near its words -- asking which country a product comes from finds the
    /// country recorded against its supplier, one hop away.
    #[tool(name = "link")]
    fn link(&self, Parameters(p): Parameters<LinkParams>) -> Result<Json<WriteResult>, ErrorData> {
        let at = parse_at(p.at.as_ref())?;
        let outcome = self.with(|b| b.link(&p.from, &p.rel, &p.to, at))?;
        Ok(Json(WriteResult {
            outcome: outcome.kind().to_string(),
            fact: json!(outcome.fact()),
            // `link` is always a relation, so the mistake this warns about cannot
            // be made here.
            hint: None,
        }))
    }

    /// Search with a natural-language question. Use this when you do not already
    /// know the exact subject and predicate; use `get` when you do.
    ///
    /// Answers with what is currently true by default. `as_of` travels back to
    /// an instant, `history` returns closed intervals too. A retracted fact
    /// appears in none of them: it was never true, so replaying it would be a
    /// lie.
    ///
    /// Ask before answering from assumption about anything project-specific --
    /// a port, an owner, a price, a deadline. The brain may already hold it, and
    /// it will be more current than your guess.
    #[tool(name = "recall")]
    fn recall(
        &self,
        Parameters(p): Parameters<RecallParams>,
    ) -> Result<Json<RecallResult>, ErrorData> {
        let mut q = RecallQuery::new(&p.query).limit(p.limit.unwrap_or(10));
        if let Some(t) = parse_at(p.as_of.as_ref())? {
            q = q.as_of(t);
        }
        if p.history {
            q = q.history();
        }
        if let Some(s) = &p.scope {
            q = q.scope(s);
        }

        let (hits, learned) = self.with(|b| {
            let hits = b.recall(&q)?;
            // After the answer is settled, never before: the ranking returned is
            // the ranking a replay would produce.
            let learned = if p.learn {
                b.learn_alias(&p.query, &hits)?
            } else {
                None
            };
            Ok((hits, learned))
        })?;

        Ok(Json(RecallResult {
            hits: json!(hits),
            learned: json!(learned),
        }))
    }

    /// List which subjects hold a predicate, and what their value is. This is the
    /// tool for a question about a **set** rather than about one thing: which
    /// tasks are open, which services a person owns, what is due this week.
    ///
    /// Use it instead of `recall` whenever you need *all* of something. `recall`
    /// ranks by relevance and returns the best few, and cannot tell you whether
    /// it left anything out; this returns everything that matches and reports
    /// `matched`, so you can act on the set as a set. Use `get` when you already
    /// know which subject you mean.
    ///
    /// Defaults to what is true now. A fact dated in the future is not "now" and
    /// will not appear until its time.
    #[tool(name = "which")]
    fn which(
        &self,
        Parameters(p): Parameters<WhichParams>,
    ) -> Result<Json<WhichResult>, ErrorData> {
        let mut q = WhichQuery::new(&p.predicate).limit(p.limit.unwrap_or(200));
        q.desc = p.desc;
        match (&p.value, &p.entity) {
            (Some(_), Some(_)) => {
                return Err(ErrorData::invalid_params(
                    "pass `value` or `entity`, not both",
                    None,
                ));
            }
            (Some(v), None) => q = q.value(Object::parse_literal(v)),
            (None, Some(e)) => q = q.value(Object::entity(e)),
            (None, None) => {}
        }
        if let Some(o) = &p.order_by {
            q = q.order(Order::parse(o).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("expected `subject`, `value` or `since`, got {o:?}"),
                    None,
                )
            })?);
        }
        if let Some(t) = parse_at(p.as_of.as_ref())? {
            q = q.when(When::AsOf(t));
        }
        if p.history {
            q = q.when(When::History);
        }
        if let Some(s) = &p.scope {
            q = q.scope(s);
        }

        let set = self.with(|b| b.which(&q))?;
        Ok(Json(WhichResult {
            facts: json!(set.facts),
            matched: set.matched,
            truncated: set.truncated,
        }))
    }

    /// Read one value directly, by subject and predicate. Returns what currently
    /// holds, or what held at `as_of`.
    ///
    /// `facts` carries every open value, which matters for a multi-valued
    /// predicate; `fact` is the single answer most callers want. Both are empty
    /// when nothing is known -- which is a real answer, and better than guessing.
    #[tool(name = "get")]
    fn get(&self, Parameters(p): Parameters<GetParams>) -> Result<Json<ValueResult>, ErrorData> {
        let at = parse_at(p.as_of.as_ref())?;
        let facts = self.with(|b| match at {
            Some(t) => Ok(b
                .as_of(&p.subject, &p.predicate, t)?
                .into_iter()
                .collect::<Vec<_>>()),
            None => b.current_all(&p.subject, &p.predicate),
        })?;
        Ok(Json(ValueResult {
            fact: json!(facts.first()),
            facts: json!(facts),
        }))
    }

    /// The full trajectory of one subject and predicate, oldest first, including
    /// retracted claims.
    ///
    /// Retractions stay visible here on purpose: "we believed 10, then corrected
    /// it" is part of the record, and hiding it would make the audit trail lie.
    /// Read this before overwriting something you did not write.
    #[tool(name = "history")]
    fn history(
        &self,
        Parameters(p): Parameters<PairParams>,
    ) -> Result<Json<FactsResult>, ErrorData> {
        let facts = self.with(|b| b.history(&p.subject, &p.predicate))?;
        Ok(Json(FactsResult {
            facts: json!(facts),
        }))
    }

    /// Everything known about one entity, and optionally what it connects to.
    ///
    /// Use it before writing, to check which spelling an entity already exists
    /// under. Identity is exact: `preço` and `preco` are two different entities,
    /// and a brain that grows two parallel histories for one thing cannot be
    /// repaired. Search is forgiving, writing is not.
    #[tool(name = "entity")]
    fn entity(
        &self,
        Parameters(p): Parameters<EntityParams>,
    ) -> Result<Json<EntityResult>, ErrorData> {
        let when = match parse_at(p.as_of.as_ref())? {
            Some(t) => When::AsOf(t),
            None => When::Now,
        };
        let depth = if p.neighbors {
            crate::graph::MAX_DEPTH
        } else {
            0
        };
        let view = self.with(|b| b.entity(&p.name, when, depth))?;
        Ok(Json(EntityResult {
            entity: json!(view),
        }))
    }

    /// Where a fact came from and what became of it: what it replaced, and what
    /// replaced it. Use it before retracting anything, and to explain an answer
    /// that looks wrong.
    #[tool(name = "why")]
    fn why(
        &self,
        Parameters(p): Parameters<FactIdParams>,
    ) -> Result<Json<ProvenanceResult>, ErrorData> {
        let prov = self.with(|b| b.why(p.fact_id))?;
        Ok(Json(ProvenanceResult {
            fact: json!(prov.fact),
            superseded_by: json!(prov.superseded_by),
            supersedes: json!(prov.supersedes),
        }))
    }

    /// Mark a fact as never having been true.
    ///
    /// This is not how you record a change. If the value simply changed, call
    /// `remember` with the new one and the old one stays true for its period.
    /// Retract only when the claim was wrong to begin with.
    ///
    /// It does not reopen whatever the retracted fact closed: "this was wrong"
    /// leaves that period genuinely unknown, which is more honest than inventing
    /// an answer for it.
    #[tool(name = "retract")]
    fn retract(
        &self,
        Parameters(p): Parameters<RetractParams>,
    ) -> Result<Json<RetractResult>, ErrorData> {
        let fact = self.with(|b| b.retract(p.fact_id, p.reason.as_deref()))?;
        Ok(Json(RetractResult {
            retracted: json!(fact),
        }))
    }

    /// Teach the brain that an entity also goes by another name, because someone
    /// said so.
    ///
    /// A declared name decides identity: later facts written under it land on the
    /// same entity. Use it when a person calls something by a name the brain does
    /// not know, and refuse to guess -- a wrong alias merges two histories, and
    /// that cannot be undone cleanly.
    #[tool(name = "alias")]
    fn alias(
        &self,
        Parameters(p): Parameters<AliasParams>,
    ) -> Result<Json<AliasResult>, ErrorData> {
        let alias = self.with(|b| b.declare_alias(&p.entity, &p.alias))?;
        Ok(Json(AliasResult {
            entity: p.entity,
            alias: json!(alias),
        }))
    }
}

#[rmcp::tool_handler]
impl ServerHandler for BrainServer {
    fn get_info(&self) -> ServerInfo {
        // The name is set explicitly. `Implementation::from_build_env()` reports
        // whichever crate the macro expanded in, which here is `rmcp` -- an
        // agent listing its servers would see the SDK's name, not the brain's.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                "claudinio-brain",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(format!(
                "Bitemporal memory. Facts live on a timeline: writing a new value \
                 closes the old one instead of overwriting it, so `get` answers \
                 what holds now and `get` with `as_of` answers what held then.\n\n\
                 Connected to brain {label:?} (id {id}, at {path}). Every tool in \
                 this session reads and writes that one brain.\n\n\
                 Before answering from assumption about anything project-specific \
                 -- a port, an owner, a price, a deadline -- call `recall`. Before \
                 writing about something that may already exist, call `entity` to \
                 check which spelling it is stored under. If a value changed, call \
                 `remember`; only call `retract` when a claim was never true.\n\n\
                 When a value names something you could ask another question about \
                 -- a class, an owner, a supplier, a category -- pass it as \
                 `entity`, not `value`. That records a relation, and relations are \
                 the only thing the graph can be walked through: they are what lets \
                 an answer be found near a question that never named it. Passing \
                 such a value as `value` succeeds, reads back correctly, and is \
                 invisible to retrieval forever.",
                label = self.label,
                id = self.id,
                path = self.path,
            ))
    }
}
