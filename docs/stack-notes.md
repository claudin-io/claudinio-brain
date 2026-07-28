# Stack notes

Findings from the Passo 0 spike. These are the things that cost time to
rediscover, recorded so they don't have to be.

## Verified working (macOS arm64, rustc 1.96)

| | version |
|---|---|
| bundled SQLite | 3.53.2 |
| sqlite-vec | v0.1.9, statically linked, no `.dylib` |
| FTS5 + JSON1 + recursive CTE | present in the bundled amalgamation, no feature flag |

## No C++ in the tree

`model2vec-rs` → `tokenizers 0.21` pulls `esaxx-rs`, whose default `cpp` feature
compiles C++. Declaring `tokenizers` as a **direct dependency** with
`default-features = false, features = ["onig"]` makes Cargo's feature unification
supply the Oniguruma regex backend (C) without `esaxx_fast` (C++).

Verified: `esaxx-rs`'s build-script `out/` directory is empty and no `.cpp.o`
exists anywhere in `target/`. A C compiler is required (onig, sqlite3, sqlite-vec);
a C++ compiler is not.

If this ever breaks, the fallback is `model2vec-rs/features = ["onig","local-only"]`,
accepting the C++ dep — fine on macOS, painful for musl cross-builds.

## MSRV

Declared `rust-version = "1.88"`, not edition 2024's 1.85 floor: `schemars` →
`darling 0.23` requires 1.88.

## SQLite gotchas found the hard way

**`bm25()` is an FTS5 auxiliary function.** It only works in the `SELECT` or
`ORDER BY` of a query that `MATCH`es directly. Nesting it in an aggregate
(`min(bm25(t))`) fails with "unable to use function bm25 in the requested
context". Select it per row and aggregate in Rust.

**`bm25()` returns negative scores** — more negative is a better match. Negate
before feeding RRF.

**FTS5 IDF degenerates on tiny corpora.** With 2 documents where 1 matches,
`log((N-n+0.5)/(n+0.5)) = log(1) = 0`, so bm25 returns exactly `0.0`. Test
fixtures need several non-matching rows before ranking assertions mean anything.
This will bite in unit tests, not in production.

**vec0 cannot infer element type from a bare blob.** A 4-byte blob for an
`int8[4]` column is read as `float32[1]` and rejected. Every write *and* every
`MATCH` must wrap the parameter in `vec_int8(?)`.

**vec0 metadata filters do not support `IS NULL`**, `LIKE`, `GLOB` or `REGEXP`.
"Currently valid" is therefore modelled as an `is_open INTEGER` column, not as a
nullable `valid_to`.

**Recursive CTEs need `UNION`, not `UNION ALL`, to terminate on cycles.** The
graph will have cycles; `UNION` deduplicates and halts, `UNION ALL` does not.

## Test hermeticity

**`directories` ignores XDG on macOS** — it resolves to `~/Library/Application
Support`, so a process-level test would read and write the developer's real
config. `Ctx::from_process` therefore honours `BRAIN_CONFIG_DIR` and
`BRAIN_DATA_DIR` overrides, and every CLI test sets both plus `env_clear()`.

**`TempDir` paths are not canonical on macOS.** `/var` is a symlink to
`/private/var`, and `std::env::current_dir()` returns the *resolved* form. A test
that compares a resolved path against a raw `TempDir` path fails on macOS and
passes on Linux. CLI test sandboxes canonicalize their root at construction.

Note that `locate` deliberately does *not* canonicalize the brain path it
returns: it respects the path the user actually gave. Brain identity comes from
the `brain_id` stored in the file's `meta` table, not from its path string.

## Binary size baseline

2.1 MB release (LTO fat, stripped) — but this is a *floor*, not an estimate: the
spike binary references almost none of the dependency surface, so LTO drops it.
The `mcp` feature made no measurable difference for the same reason. Real numbers
come once Passo 5 links the model weights (~8 MB for potion-base-8M int8) and
Passo 8 actually exercises `rmcp`.
