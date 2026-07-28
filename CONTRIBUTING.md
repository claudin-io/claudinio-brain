# Contributing to Claudinio Brain

Thanks for taking the time. This covers what you need to get a build running and
what a mergeable change looks like.

## Before you write code

- **Bugs** — open an issue with the reproduction. If you already have the fix,
  open the PR and link the issue.
- **Features** — open an issue first and describe the problem before the
  solution. The bitemporal model, the retrieval channels and the isolation rules
  have opinions baked into them, and a short discussion saves you from rewriting
  a large PR.
- **Retrieval quality** — if the change is "recall should rank X higher", bring
  an eval case (see below). A ranking argument without a case is unfalsifiable.
- **Small stuff** — typos, broken links, obvious one-line fixes: just send the PR.

## Development setup

### Prerequisites

| Tool | Version |
|---|---|
| [Rust](https://rustup.rs) | 1.95+, edition 2024 |
| A C compiler | for `onig`, bundled SQLite and `sqlite-vec` |

A **C++** compiler is deliberately not required. `tokenizers` is declared as a
direct dependency with `default-features = false, features = ["onig"]` precisely
so Cargo's feature unification keeps `esaxx-rs` off its C++ path. If you find
yourself needing `g++` to build this, something in the dependency tree changed
and that is worth an issue.

```bash
git clone https://github.com/claudin-io/claudinio-brain.git
cd claudinio-brain
cargo build
cargo test
```

The first build compiles SQLite and links 7.3 MB of embedding weights. Expect it
to take a while; incremental builds are fast.

## Checks your PR must pass

Run these locally before pushing — CI runs the same set:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo check --no-default-features    # the core must build without rmcp/tokio
cargo run --example eval             # recall quality, gated on the baseline
```

The tree is warning-clean. A PR that adds warnings will fail CI. CI additionally
builds on macOS arm64, linux-gnu and linux-musl, and checks against MSRV 1.95.

The MSRV is measured rather than inherited: `cargo metadata` reports 1.88 as the
highest floor any dependency *declares*, but `libsqlite3-sys` uses `cfg_select`
in its build script without declaring an MSRV at all. If you raise it, verify by
building on the toolchain you are declaring, not by reading the manifests.

## Tests and evals are different things

Both are required, and neither substitutes for the other.

**Tests** prove correctness — does the invariant hold? They live in `tests/`,
named by the step that introduced them. Everything is hermetic: no network, no
real `$HOME`. The clock and the id generator are injected traits, so nothing
reads ambient time or randomness. If your test needs to know what time it is,
that is a design problem in the code under test, not in the test.

Invariants that must hold over *arbitrary* write orders belong in the proptest
block (`tests/step3_invariants.rs`, `tests/step6_invariants.rs`) rather than as
one hand-picked sequence.

**Evals** measure quality — does it find the right fact? They live in
`evals/*.jsonl`, one JSON object per line. Adding a case is usually a one-line
diff:

```json
{"name":"what it tests","facts":[...],"query":"the question","expect":["a"]}
```

`cargo run --example eval` scores every suite against every channel in isolation
and fused, and fails if any metric falls below `evals/baseline.json`. If your
change legitimately improves a number, update the baseline **in the same commit**
so the new number lands in the diff and gets reviewed. Use `--misses` to see
which cases are still wrong.

Add noise facts to your eval cases. FTS5's IDF degenerates on tiny corpora: with
two documents where one matches, bm25 returns exactly 0.0 and your ranking
assertion means nothing.

## Things the review will ask about

These are the project's standing opinions. Going against one is allowed, but say
why in the PR.

- **No constant without a measurement.** Every threshold in `src/recall.rs` and
  `src/alias.rs` carries a comment showing the sweep that chose it and what moved.
  If a suite cannot price your constant, say *that* — `evals/README.md` has a
  section for exactly this, and borrowing the suites' authority for a number they
  did not decide is worse than admitting the gap.
- **Do not tune until one visible case flips.** A weight adjusted until a
  specific eval case passes is overfitting to the eval set. Two suites already
  carry a permanently-failing case for this reason.
- **Identity is exact; search is forgiving.** `norm::key` decides identity and
  preserves accents. Accent folding belongs in FTS5's `remove_diacritics`. A
  brain that grows two parallel histories for one thing is unrecoverable.
- **Nothing guessed decides where a fact is written.** Retrieval may guess;
  identity may not.
- **stdout belongs to the user's output.** `clippy::print_stdout` is denied
  crate-wide. Diagnostics go to stderr via `tracing`. In MCP mode stdout is the
  JSON-RPC transport, and one stray `println!` corrupts it with no diagnostic.
- **A schema change is a real change.** There is no migration path yet, so
  bumping `SCHEMA_VERSION` makes existing brains unreadable. Call it out.

## Codebase map

```
src/
  store/          opening a brain and keeping it sealed; schema.sql lives here
  brain/          the bitemporal core -- supersede, correct, retract, reassert
  recall.rs       the four channels and RRF fusion
  graph.rs        traversal, bounded by depth, hub blocking and cycle-safe CTEs
  alias.rs        declared and learned names, and the line between them
  embed/          static embeddings; the weights are include_bytes!'d
  index/          vector search: vec0 plus a brute-force conformance fallback
  norm.rs         text -> identity key. The whole deduplication story
  locate.rs       which brain an invocation means, and why
  cli.rs main.rs  the command surface
evals/            quality suites and the committed baseline
docs/stack-notes.md   findings that cost time to rediscover
```

[docs/stack-notes.md](docs/stack-notes.md) is worth reading before you fight
SQLite. It records the non-obvious things: why `bm25()` cannot be nested in an
aggregate, why `vec0` needs `vec_int8(?)` on every write and match, why recursive
CTEs need `UNION` rather than `UNION ALL`, and why `directories` ignores XDG on
macOS.

## Commits

Conventional commits (`feat(scope):`, `fix:`, `docs:`). The body matters more
than the subject: explain the decision and what you measured, not the diff.
Reviewers can read the diff.

Do not add `Co-Authored-By` trailers.
