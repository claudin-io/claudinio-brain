# Stack notes

Findings from the Passo 0 spike. These are the things that cost time to
rediscover, recorded so they don't have to be.

## Verified working (macOS arm64, rustc 1.96)

| | version |
|---|---|
| bundled SQLite | 3.53.2 |
| sqlite-vec | v0.1.9, statically linked, no `.dylib` |
| FTS5 + JSON1 + recursive CTE | present in the bundled amalgamation, no feature flag |

## Two dependency "updates" that must never be taken

Both were proposed by Dependabot within minutes of the repository going public,
and both are traps rather than upgrades.

**`tokenizers` is not a dependency.** It is a feature-unification lever, and its
version has to track `model2vec-rs`, not crates.io. See the next section for what
it is levering. Bumping it to 0.23 while `model2vec-rs` still requires `^0.21`
puts two versions in the graph, unification stops reaching the older one, and it
fails with *"One of the `onig`, or `fancy-regex` features must be enabled"* --
because `model2vec-rs` declares `tokenizers` with no regex backend at all and
relies on someone else to supply one.

**`dtolnay/rust-toolchain@1.95.0` in the `msrv` job is the MSRV.** Dependabot
sees a version-shaped ref and offers 1.100.0. Taking it would leave the job
green while testing nothing, which is the worst possible outcome for a check
whose only purpose is to fail. The three other uses of that action are `@stable`
and are fine to bump.

Both are in `.github/dependabot.yml`'s ignore lists with the reasoning inline,
because a bare `ignore:` entry invites someone to helpfully remove it later.

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

Declared `rust-version = "1.95"`, and the number came from building, not from
reading manifests.

`cargo metadata` reports 1.88 as the highest floor any dependency *declares*
(`schemars` → `darling 0.23`), and that is what was declared originally. It was
wrong. `libsqlite3-sys 0.38.1` calls `cfg_select!` in its build script and
declares no `rust-version` at all, so nothing in the metadata reflects it.
`cfg_select` was unstable through 1.94:

| toolchain | result |
|---|---|
| 1.88 – 1.94 | `error[E0658]: use of unstable library feature 'cfg_select'` |
| 1.95, 1.96 | builds |

There is no downgrade path: `rusqlite 0.40.1` requires `libsqlite3-sys ^0.38.1`,
and 0.38.0 is not a resolvable alternative.

The general lesson, which cost the first CI run this repository ever had: a
declared MSRV is a claim about a *build*, and the only way to check it is to run
one. A build-script dependency can raise the real floor without any manifest
saying so.

## musl and `sqlite-vec`'s BSD typedefs

`sqlite-vec.c` lines 68-70 are:

```c
#ifndef _WIN32
#ifndef __EMSCRIPTEN__
#ifndef __COSMOPOLITAN__
#ifndef __wasi__
typedef u_int8_t uint8_t;
typedef u_int16_t uint16_t;
typedef u_int64_t uint64_t;
#endif
```

Nothing in that guard excludes musl, and musl does not declare the BSD spellings,
so the target dies at `unknown type name 'u_int8_t'` before any Rust compiles.

`-D_GNU_SOURCE` is the obvious guess and it is wrong — measured, not assumed.
Even with the BSD names declared, the typedef still collides with `stdint.h`'s
`uint8_t`. What works is mapping the BSD names onto the standard ones, which
turns those three lines into self-typedefs, legal since C11:

```
CFLAGS_x86_64_unknown_linux_musl=-Du_int8_t=uint8_t -Du_int16_t=uint16_t -Du_int64_t=uint64_t
```

Those three lines are the only occurrences of `u_int` in the file, so the
substitution cannot reach anything else.

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
