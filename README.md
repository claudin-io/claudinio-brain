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

Give an agent a vector store and ask how a service authenticates. It will find
every answer anyone ever wrote down and pick one. It has no way to know which of
them is still true, because "we use JWT" and "we *used* JWT" are the same
sentence to a similarity score.

`brain` stores facts on a timeline instead of in a pile. Writing a new value does
not overwrite the old one — it closes it. So one record answers both questions:

```console
$ brain remember --subject auth --predicate strategy --value "JWT" \
    --at 2026-01-01 --source adr-004
created: auth strategy JWT

$ brain remember --subject auth --predicate strategy --value "server-side sessions" \
    --at 2026-06-01 --source adr-011
superseded: auth strategy server-side sessions

$ brain get auth strategy
auth strategy server-side sessions

$ brain get auth strategy --as-of 2026-03-01
auth strategy JWT

$ brain history auth strategy
[2026-01-01T00:00:00Z] auth strategy JWT  (until 2026-06-01T00:00:00Z)
[2026-06-01T00:00:00Z] auth strategy server-side sessions  (current)
```

Nothing was deleted, and nothing had to be re-embedded to make that work.

## A fact is anything worth more than one session

Subject, predicate, value. Nothing in the model is domain-specific — there is no
schema to declare, and a predicate is just a word:

```console
$ brain remember --subject api_gateway --predicate timeout --value 30 --unit s
$ brain remember --subject checkout_service --predicate owner --value "platform-team"
$ brain remember --subject release_1_4 --predicate freeze_date --value 2026-08-15
$ brain remember --subject André --predicate team --value "platform"
```

A decision and the reason for it, a config value, an owner, a deadline, a schema
version, a constraint someone stated out loud, where in the codebase the real
answer lives. Anything an agent would otherwise re-derive, guess at, or lose when
the session ends.

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

## A fact can carry its own end

`valid_to` used to be something only a *second* fact could write: you ended a
claim by superseding it. That quietly decided what the brain was for, because
anything whose end was known in advance — a freeze that lifts on Friday, a token
good for an hour, a state nobody will come back to correct — had to be revisited
by hand or left out entirely.

```console
$ brain remember --subject release_1_4 --predicate freeze --value on --until 2026-08-15
created: release_1_4 freeze on

$ brain get release_1_4 freeze                      # before the 15th
release_1_4 freeze on

$ brain get release_1_4 freeze                      # after it, nothing written in between
(nothing known)

$ brain history release_1_4 freeze
[2026-08-01T00:00:00Z] release_1_4 freeze on  (until 2026-08-15T00:00:00Z)
```

Nobody wrote anything and the answer changed. Reasserting with a *later*
`--until` pushes the end back, so a short-lived fact stays alive by repetition
instead of being rewritten; an earlier one is ignored, so a heartbeat can never
kill the thing it was sent to keep alive. And an end that would run past the
claim following it is clipped to where that one starts — `--until` narrows and
never widens, because two facts covering one instant is the single thing this
timeline exists to prevent.

**"Now" means now.** The value that holds is the one whose interval contains this
instant, which is exactly `--as-of` against the clock. A price announced for
January is the newest thing the brain knows and is not today's price; a freeze
that lifted on Friday is still the newest thing it knows on Saturday. Both used
to read as current.

## Relations are facts

`link A rel B` is a fact whose object is an entity, so the graph inherits
bitemporality for free — a dependency that moved is a closed interval, not a
deleted row. It also means the answer to a question can live somewhere the
question's words never reach:

```console
$ brain link checkout_service depends_on payments_db
$ brain remember --subject payments_db --predicate region --value "eu-west-1"

$ brain recall "which region does checkout_service data live in" --limit 3
checkout_service owner platform-team
payments_db region eu-west-1
checkout_service depends_on payments_db
```

"eu-west-1" shares no word with the question, and neither does `payments_db`,
which the question never names. It is one hop past the entity that *was* named,
and the relation is the map to it.

## How recall works

Five independent channels retrieve candidates, and reciprocal rank fusion
combines them. Fusing rather than picking one is the point: agreement between
independent signals is itself evidence.

| channel | finds |
|---|---|
| **bm25** | words, over FTS5. Accent-insensitive, so "andre" finds "André". |
| **alias** | entities the question names outright, by key or by another name. |
| **semantic** | paraphrases, via static embeddings compiled into the binary. |
| **graph** | facts reached by walking relations out from what the question named. |
| **kin** | facts on entities that merely have something *in common* with it. |

The last one exists because almost nothing in a real brain has an edge to its
siblings. Twenty vouchers each recording `is_a seasonal_voucher` are a cohort
nobody ever drew, and two entities are kin when they hold the same
`(predicate, object)` pair — no edge required, and the value may be a plain
string. Rarity does the ranking: a pair five entities share says far more about
which of them matters than one twenty-two share, and inverse document frequency
is what sorts them.

Everything is filtered temporally *before* ranking, so recall answers with what
is true rather than with everything ever recorded. A retracted fact appears in
neither `--as-of` nor `--history`: it was never true, so replaying it would lie.

The semantic channel is a **static embedding** table — a token-to-vector lookup,
not a transformer. No ONNX runtime, no download, no C++ toolchain, and no
sampling, which is what makes recall reproducible enough for the eval baselines
to exist at all.

## Asking about a set

`get` needs a subject and `recall` guesses at one, so neither can answer *which
things* — which services a team owns, what is still open, what has no source.
That is a different shape of question, not a harder version of the same one, and
no amount of ranking produces it.

```console
$ brain which status open
fix_login status open
write_docs status open

$ brain which due --order-by value          # ISO-8601 sorts chronologically
write_docs due 2026-03-09
fix_login due 2026-08-15

$ brain which owner --entity platform-team  # by identity, so other names come along
checkout_service owner platform-team
```

What separates this from `recall` is not the phrasing, it is what the answer
promises. `recall` ranks, returns the best few, and has no way to tell you what it
left out. `which` returns everything that matches and reports `matched` beside it,
so an agent can tell *these are the open tasks* from *these are ten of the open
tasks* and act on the set as a set. Select the way you wrote it: `--value` for a
literal, `--entity` to match by identity.

### Things that churn

A set query and a self-closing fact are between them enough to keep a task list
in a brain, which the old advice said not to do. That advice was aimed at the
wrong thing: a task that reaches `done` is not transient, it is a closed interval
with a duration, an owner and a reason — the part a checkbox throws away. What was
missing was a way to read the list back.

```console
$ brain remember --subject fix_login --predicate status --value open --scope todo
$ brain which status open --scope todo
$ brain recall "how do we authenticate" --not-scope todo
```

Nothing is ever deleted here, and every fact lands in the same two indexes that
every question is searched through, so a scope is how high-churn work is kept from
competing with the durable knowledge forever. `--not-scope` is the half that was
missing: `--scope` could only ever narrow inward.

One trap worth naming. `brain lint` will notice that several tasks share
`status open` and suggest promoting `open` to a node so things can be reached
through it. Don't: traversal deliberately refuses to expand *through* a
high-degree hub, so the graph answer works up to about fifty tasks and then
silently returns nothing at all. That is the failure `which` exists to replace.

## Names

An entity is stored under one key, but people ask about it in other words.

```console
$ brain alias payments_db "the payments database"     # a name you declare
"the_payments_database" now names payments_db

$ brain recall "which team is andre on" --learn --limit 3
André team platform
checkout_service depends_on payments_db
checkout_service owner platform-team
(learned: "andre" names André)

$ brain entity "André"
André (andré)
  also: andre (learned)
  André team platform
```

The question dropped an accent, so it named nothing — identity is exact, and
`andre` is not `andré`. BM25 answered anyway, because search is the forgiving
layer, and the name that worked was kept.

The two are trusted very differently, and the split is load-bearing:

- A **declared** alias decides identity. Later facts about "the payments
  database" land on `payments_db`.
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

A prebuilt binary. No Rust toolchain, no C compiler, nothing to build:

```bash
curl -fsSL https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.ps1 | iex
```

It lands in `~/.local/bin` (`%LOCALAPPDATA%\Programs\brain` on Windows) and the
download is checked against the release's `SHA256SUMS` before it is installed.
`BRAIN_INSTALL_DIR` chooses somewhere else; `BRAIN_VERSION` chooses a release —
`nightly` tracks `main`, rebuilt on every push.

| platform | build |
|---|---|
| macOS | `aarch64-apple-darwin`, `x86_64-apple-darwin` |
| Linux | `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` |
| Windows | `x86_64-pc-windows-msvc` |

The Linux builds are static musl, so they have no glibc floor and run on old
distros and on Alpine alike.

### From source

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
which      List which subjects hold a predicate, and what the value is
history    Show the full trajectory of a subject/predicate pair
entity     Show what is known about an entity, and what it connects to
why        Show where a fact came from and what became of it
retract    Mark a fact as never having been true
alias      Give an entity another name, list the names it has, or take one away
lint       Report what is structurally wrong: relations stored as strings,
           entities nothing can reach, one thing living under two names
repair     Fix how facts are stored, without changing what they say
reindex    Rebuild the vector index from the stored embeddings
predicate  Fix what a predicate is: how many values it holds at once, and
           whether its object names a thing rather than being a literal
studio     Open the brain in a 3D viewer and editor, served from localhost
export     Write the brain to a single self-contained HTML file
```

Every command takes `--json`, and every JSON answer is stamped with the brain
that produced it. `--brain <path>`, `--use <name>` and `--global` select which
brain to talk to.

## The studio

A graph on two timelines does not read well as a list. `brain studio` opens one
in a browser, in 3D, served from localhost:

![The studio: a 3D graph of a coffee roaster's brain, a recall trace showing
which of the five channels found each hit, an inspector listing one supplier's
facts and names, and the bitemporal plane along the bottom](docs/studio.png)

```bash
brain studio        # a live editor on 127.0.0.1; writes go to the brain file
brain export        # the same page as one HTML file, read-only, works offline
```

A node is an entity. An edge is a fact whose object is another entity — so edges
carry both time axes like everything else does. An edge that **ended** is drawn
as a dashed ghost rather than deleted, one that is **expiring** carries its own
end and is still the answer until it arrives, one **not yet true** is dated ahead
and kept in view anyway, and one that was **retracted** is hidden unless you ask,
because it was never true.

Five parts do the work:

**The valid-time ruler.** Drag it and the graph re-forms into whatever was true
at that instant; relations appear and vanish under the cursor. It is `--as-of` as
a gesture, and the predicates behind it mirror `TemporalFilter` in
`src/recall.rs` exactly. A debugger that filters time even slightly differently
from the thing it is debugging invents disagreements and hides real ones.

**The bitemporal plane.** x is when a fact was true; y is when the brain was
told. Drag the horizontal cursor down and the brain's own knowledge rewinds —
what did it believe *before* the correction landed? Nothing extra is stored to
make that work: a fact's closure was learned when the fact that closed it was
recorded, and `superseded_by` is the pointer to it. This is also the only view
where the three write outcomes look like three different things. A supersession
is a bar that stops and another starting higher up. A correction is a struck bar
with nothing taking its place. A reassertion is one bar that got thicker instead
of a second bar appearing.

**The recall trace.** Ask a question and the channels colour the nodes they
surfaced, with each hit's channels and fused score beside it. A fact several
channels agree on is drawn in their average — which is the agreement RRF rewards,
made visible. In the screenshot above, "de que pais vem o bourbon amarelo" is
answered by `Fazenda Serra Azul pais Brasil`, and the `graph` chip on it is the
walk that got there: the country is one hop past the entity the question named.

**The set query.** Click a predicate in the rail and every subject holding it is
selected, in the graph and as a list. This is the one question a graph cannot be
read for — a node shows what it holds, and nothing shows which nodes hold a given
thing — so it was as invisible here as it was unaskable at the CLI. The list says
"all of them", which is the property that separates it from the recall trace above
it.

**The editor.** In `brain studio` only. Recording a value does not overwrite the
old one and you watch the interval close as a new one opens, so the model teaches
itself. Every write reports which of the four outcomes it was, decided in the
core rather than guessed at in the browser.

### Try it

```bash
sh examples/demo.sh              # a brain with a price superseded twice and a
                                 # fourth value dated in the future, a supplier
                                 # that changed, a fact that was never true, and
                                 # a declared alias next to a learned one
brain --brain demo/brain.db studio
```

### What it costs

three.js is vendored into the repository and compiled into the binary, so an
exported page opens from `file://` with no server, no CDN and no network — the
same promise the binary makes. `tools/vendor-three.sh` regenerates it and needs
node only when it is run, never to build or use `brain`. Building with
`--no-default-features --features mcp` leaves the studio out entirely.

The page declares `default-src 'none'`, so the browser *enforces* that it fetches
nothing rather than taking it on trust. The server binds loopback only, and a
write needs both a token in `X-Brain-Token` — not a CORS-safelisted header, so a
page in another tab cannot send one — and a `Host` that is literally loopback,
which is what stops DNS rebinding from making the token travel for free.

## MCP

`brain serve` speaks MCP over stdio, so an agent can use a brain as a tool
surface. Nine tools: `remember`, `link`, `recall`, `get`, `history`, `entity`,
`why`, `retract`, `alias`.

```json
{
  "mcpServers": {
    "brain": { "command": "brain", "args": ["serve", "--global"] }
  }
}
```

The server resolves its brain once at startup, through the same eight-rung
ladder as everything else, and stays bound to it for the session — so the
identity is stated once in the server instructions rather than stamped on every
response, unlike the CLI where each invocation could name a different file.

Tool descriptions carry the guidance that is easy to get wrong: that a value
which *changed* calls for `remember` and one that was *never true* calls for
`retract`; that `entity` tells you which spelling something is already stored
under, because identity is exact and two parallel histories cannot be repaired.

`recall` does not learn names unless asked. A read that writes is a read that
cannot be replayed.

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
cargo run --example eval -- --holdout         # ...and score what nothing is tuned against
```

Five suites — retrieval, temporal, graph, alias, kin — each scored against every
channel alone and fused, so the marginal contribution of a channel is a number
rather than an opinion. `evals/baseline.json` is committed and CI fails on
regression, which means improving a number requires updating the baseline in the
same commit, where it lands in the diff and gets reviewed.

A sixth suite, `holdout.jsonl`, is the control. It is scored and gated like the
others but **never names a failing case**, because a case you can see is a case
you can nudge a constant until it passes. Everything the visible suites claim
about a ranking change is worth exactly as much as what the holdout says about
the same change.

[evals/README.md](evals/README.md) explains how to read the ablation table, and
has a section on what these suites *cannot* measure.

## Status

Pre-1.0. The on-disk format is not yet stable and there is no migration path
between schema versions.

Built and working: the sealed store and resolution ladder, the bitemporal fact
model, all five retrieval channels, declared plus learned names, the MCP server,
and the studio. Four surfaces — the CLI, the MCP server, the studio and the Rust
library — all over one core, so what an agent sees is exactly what `brain recall`
shows you and exactly what the graph draws.

## Contributing

[CONTRIBUTING.md](CONTRIBUTING.md) covers the setup and what a mergeable change
looks like. Security reports go through [SECURITY.md](SECURITY.md) — please do
not open a public issue for those.

## License

MIT. See [LICENSE](LICENSE).
