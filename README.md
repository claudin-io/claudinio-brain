<h1 align="center">Claudinio Brain</h1>

<p align="center">
  <strong>Bitemporal knowledge-graph memory for AI agents.</strong><br>
  One binary, one file, no server, and no model on the write path.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.95%2B-orange.svg">
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey">
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.pt-BR.md">Português</a>
</p>

---

Give an agent a vector store and ask it what a product costs. It will find every
price you ever wrote down and pick one. It has no way to know which of them is
still true, because "the price is 20" and "the price *was* 20" are the same
sentence to a similarity score.

`brain` stores facts on a timeline instead of in a pile. Writing a new value does
not overwrite the old one — it closes it. So one record answers both questions:

```console
$ brain remember --subject produto_a --predicate preco --value 20 --at 2026-01-01
created: produto_a preco 20

$ brain remember --subject produto_a --predicate preco --value 25 --at 2026-06-01
superseded: produto_a preco 25

$ brain get produto_a preco
produto_a preco 25

$ brain get produto_a preco --as-of 2026-03-01
produto_a preco 20

$ brain history produto_a preco
[2026-01-01T00:00:00Z] produto_a preco 20  (until 2026-06-01T00:00:00Z)
[2026-06-01T00:00:00Z] produto_a preco 25  (current)
```

Nothing was deleted, and nothing had to be re-embedded to make that work.

## Why bitemporal

Two timelines, tracked separately:

- **valid time** — when the fact was true in the world.
- **transaction time** — when the brain was told.

Keeping both is what lets a brain distinguish three things a single timestamp
collapses into one, and they mean very different things to an agent:

| outcome | meaning |
|---|---|
| **superseded** | it changed. The old value *was* true, and then stopped being. |
| **corrected** | we were wrong. The old value was never true, so it is retracted. |
| **reasserted** | we were told the same thing again. Reinforce, do not duplicate. |

A retraction is deliberately not the inverse of a supersession: it does not
reopen whatever the retracted fact closed. "This was wrong" leaves that period
genuinely unknown, and inventing an answer for it would be worse than admitting
the gap.

## Relations are facts

`link A rel B` is a fact whose object is an entity, so the graph inherits
bitemporality for free — a supplier that changed is a closed interval, not a
deleted row. It also means the answer to a question can live somewhere the
question's words never reach:

```console
$ brain link produto_a fornecido_por acme
$ brain remember --subject acme --predicate pais --value "Chile"

$ brain recall "de que pais vem o produto_a"
acme pais Chile
produto_a preco 25
produto_a fornecido_por acme
```

"Chile" shares no word with the question. It is one hop past the entity the
question names, and the relation is the map to it.

## How recall works

Four independent channels retrieve candidates, and reciprocal rank fusion
combines them. Fusing rather than picking one is the point: agreement between
independent signals is itself evidence.

| channel | finds |
|---|---|
| **bm25** | words, over FTS5. Accent-insensitive, so "preco" finds "preço". |
| **alias** | entities the question names outright, by key or by another name. |
| **semantic** | paraphrases, via static embeddings compiled into the binary. |
| **graph** | facts reached by walking relations out from what the question named. |

Everything is filtered temporally *before* ranking, so recall answers with what
is true rather than with everything ever recorded. A retracted fact appears in
neither `--as-of` nor `--history`: it was never true, so replaying it would lie.

The semantic channel is a **static embedding** table — a token-to-vector lookup,
not a transformer. No ONNX runtime, no download, no C++ toolchain, and no
sampling, which is what makes recall reproducible enough for the eval baselines
to exist at all.

## Names

An entity is stored under one key, but people ask about it in other words.

```console
$ brain remember --subject acme --predicate funcionarios --value 40
$ brain remember --subject "Produto Brasília" --predicate preco --value 20
$ brain remember --subject servidor --predicate porta --value 8080

$ brain alias acme "ACME Corp"           # a name you declare
"acme_corp" now names acme

$ brain recall "quanto custa o produto brasilia" --learn
Produto Brasília preco 20
acme funcionarios 40
servidor porta 8080
(learned: "produto_brasilia" names Produto Brasília)

$ brain entity "Produto Brasília"
Produto Brasília (produto_brasília)
  also: produto_brasilia (learned)
  Produto Brasília preco 20
```

The question dropped an accent, so it named nothing — identity is exact, and
`produto_brasilia` is not `produto_brasília`. BM25 answered anyway, because
search is the forgiving layer, and the name that worked was kept.

The two are trusted very differently, and the split is load-bearing:

- A **declared** alias decides identity. Later facts about "ACME Corp" land on
  `acme`.
- A **learned** alias only widens retrieval. A guess that could decide identity
  would let one well-phrased question graft an entity's entire future history
  onto the wrong node, with nothing in any output to show it happened.

Learning is off unless you ask for it (`--learn`), because a read that writes is
a read that cannot be replayed. `brain entity <name>` shows every name a thing
answers to and which kind each one is.

## Isolation

A brain is exactly one SQLite file, and two properties are enforced rather than
promised:

- **`ATTACH` is impossible.** `SQLITE_LIMIT_ATTACHED` is zero on every
  connection, so no query can reach a second database file. Without it, one
  crafted statement could join another tenant's facts into a result set.
- **A file is only a brain if it says so.** `open` requires a `brain_id` marker,
  so a stray `.db` is never adopted. Files are created `0600`.

Every JSON answer carries `brain_id`, `brain_label` and `brain_path` — the last
because copying a brain file duplicates its id, so identity alone cannot tell two
copies apart.

`brain where` explains which brain an invocation would use, and why. The lookup
ladder has eight rungs and no silent fallback: a directory with no brain is an
error, never the global one.

## Install

Requires a C compiler (for `onig`, bundled SQLite and `sqlite-vec`). A C++
compiler is deliberately **not** required — see [docs/stack-notes.md](docs/stack-notes.md).

```bash
cargo install --git https://github.com/claudin-io/claudinio-brain
```

Or from a clone:

```bash
git clone https://github.com/claudin-io/claudinio-brain.git
cd claudinio-brain
cargo build --release        # target/release/brain
```

The release binary is a single self-contained file of about 14 MB — SQLite, the
vector index and 7.3 MB of quantized embedding weights are all compiled in.
Nothing is downloaded at runtime, and `brain` never makes a network request:
`model2vec-rs` is built with `local-only`, which removes every network path at
compile time.

## Commands

```
init       Create a new brain
where      Show which brain would be used here, and why
stats      Report the brain's identity and contents
remember   Record a fact
link       Record a relation between two entities
get        Read the current value, or the value at a past instant
recall     Search the brain with a natural-language question
history    Show the full trajectory of a subject/predicate pair
entity     Show what is known about an entity, and what it connects to
why        Show where a fact came from and what became of it
retract    Mark a fact as never having been true
alias      Give an entity another name, list the names it has, or take one away
reindex    Rebuild the vector index from the stored embeddings
predicate  Fix a predicate's cardinality
```

Every command takes `--json`, and every JSON answer is stamped with the brain
that produced it. `--brain <path>`, `--use <name>` and `--global` select which
brain to talk to.

## Using it from an agent

`skills/claudinio-brain/` is an [Agent Skill](https://agentskills.io) that teaches
an agent when to write a fact and when to just answer — including the parts that
are easy to get wrong, like the difference between a value that *changed* and one
that was *never true*.

```bash
npx skills add claudin-io/claudinio-brain     # or copy the directory into your
                                              # agent's skills folder
```

The skill assumes `brain` is on `PATH` and makes no network requests.

## Evals

Tests prove correctness; evals measure quality. A brain can satisfy every
bitemporal invariant and still be useless if recall never surfaces the answer.

```bash
cargo run --example eval                      # measure, fail on regression
cargo run --example eval -- --misses          # ...and name the cases still wrong
```

Four suites — retrieval, temporal, graph, alias — each scored against every
channel alone and fused, so the marginal contribution of a channel is a number
rather than an opinion. `evals/baseline.json` is committed and CI fails on
regression, which means improving a number requires updating the baseline in the
same commit, where it lands in the diff and gets reviewed.

[evals/README.md](evals/README.md) explains how to read the ablation table, and
has a section on what these suites *cannot* measure.

## Status

Pre-1.0. The on-disk format is not yet stable and there is no migration path
between schema versions.

Built and working: the sealed store and resolution ladder, the bitemporal fact
model, all four retrieval channels, and declared plus learned names. An MCP
server is the next piece — the `mcp` cargo feature is declared and on by default
but nothing implements it yet, so today the CLI and the Rust library are the two
real surfaces.

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) covers the setup and what a mergeable change
looks like. Security reports go through [SECURITY.md](SECURITY.md) — please do
not open a public issue for those.

## License

MIT. See [LICENSE](LICENSE).
