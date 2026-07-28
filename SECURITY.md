# Security Policy

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report privately through [GitHub Security Advisories](https://github.com/claudin-io/claudinio-brain/security/advisories/new),
or email **security@claudin.io**.

Please include:

- what you can do with the bug (impact), not just where it is
- the version (`brain --version`, or the commit you built from)
- your OS and architecture
- reproduction steps, ideally with a minimal brain file or a failing test

You will get an acknowledgement within 72 hours and a fix or a plan within 14
days. Please give us 90 days before public disclosure. We will credit you in the
release notes unless you prefer otherwise.

## Supported versions

Only the latest commit on `main` receives fixes. There are no long-term support
branches while the project is pre-1.0, and the on-disk format is not yet stable.

## Threat model

`brain` is a **local library and CLI**. It has no server, opens no sockets and
makes no network requests. The interesting boundary is therefore not the network
— it is the one between two brains, and the one between a brain file and the
process that opens it.

### What we defend

| Boundary | Guarantee |
|---|---|
| One connection reaches one file | `SQLITE_LIMIT_ATTACHED` is set to zero on every connection (`src/store/mod.rs`), so no statement can `ATTACH` a second database, including `:memory:` and temp databases. Without this, one crafted query could join a second brain's facts into a result set. |
| No extension can re-open that seal | rusqlite's `load_extension` feature is deliberately not enabled and SQLite's own default is off, so SQL-callable `load_extension()` is unavailable. Covered by `extension_loading_is_disabled` in `tests/step2_isolation.rs`. |
| A file is only a brain if it says so | `Store::open` requires a `brain_id` marker in `meta` and never creates. A stray `.db` is refused rather than adopted or overwritten. |
| Answers name their source | Every JSON answer carries `brain_id`, `brain_label` and `brain_path`, so an agent holding two brains cannot attribute one's facts to the other. |
| Files are private by default | A new brain is created with `create_new` at mode `0600` before anything is written to it, so it is never briefly world-readable and two concurrent `init`s cannot both believe they won. |
| No query is built by string concatenation | Values reach SQLite as bound parameters. The one place free text meets a query language is the FTS5 `MATCH` expression, where every token is quoted (`fts_query` in `src/recall.rs`) so a question containing an unbalanced quote, a bare `NEAR(` or the word `AND` is a literal search term and not syntax. |
| Nothing leaves the machine | The embedding model is compiled into the binary and `model2vec-rs` is built with `local-only`, which removes every network path at compile time. There is no telemetry and no update check. |

A bug that breaks any row above is a vulnerability. Report it.

### What we explicitly do not defend

- **A brain file is not encrypted.** It is a SQLite database with mode `0600`.
  Anyone who can read the file can read every fact in it. If the facts are
  sensitive, the protection is disk encryption and file permissions, not this
  tool.
- **Local attackers with your user account.** Anyone who can already run code as
  you can read and modify the brain.
- **Copies are indistinguishable by id.** Copying a brain file duplicates its
  `brain_id`. That is why every answer also carries `brain_path`; identity alone
  cannot tell two copies apart, and it is not meant to.
- **The contents of a brain are not trusted input to *your* agent.** Facts are
  text that some earlier process wrote. If your agent executes what it reads back
  out of a brain, a poisoned fact is a prompt injection — the same as any other
  retrieved document. `brain` stores and ranks; it does not sanitize meaning.
- **A learned alias is a guess, by design.** It can be wrong. That is why it is
  confined to retrieval and never decides where a fact is written, why it is off
  unless `--learn` is passed, and why `brain entity` shows it. Widening recall
  with a wrong guess is a quality bug, not a vulnerability. A learned alias that
  ever changes write identity *is* a vulnerability.
- **Denial of service against yourself.** An unbounded pile of facts makes
  queries slow. Graph traversal is bounded (depth two, hub blocking, cycle-safe
  `UNION`), but nothing caps how much you can write.
