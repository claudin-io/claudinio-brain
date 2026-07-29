---
name: claudinio-brain
description: Give the agent durable, time-aware memory backed by the `brain` CLI — record facts, decisions, config values and relations, then recall what is true now or what was true at any past instant. Use when the user says remember this, what did we decide, what is the current value, what was it before, what changed, why did it change, or when a fact learned in one session must survive into the next. Also use before answering from assumption about a project-specific value (a port, an owner, a price, a deadline) that the brain may already hold.
license: MIT
compatibility: Requires the `brain` binary on PATH. If it is missing, this skill installs it — one prebuilt binary for macOS, Linux or Windows, no toolchain needed. Local only; `brain` itself makes no network requests.
metadata:
  author: claudin-io
  repository: https://github.com/claudin-io/claudinio-brain
  version: "0.1"
---

# Claudinio Brain

Durable memory that knows *when* things were true.

A vector store finds every price you ever wrote down and cannot tell you which
one still holds. `brain` stores facts on a timeline: writing a new value closes
the old one instead of overwriting it, so one record answers both "what is it
now" and "what was it in March".

Everything is one local SQLite file. No server, no network, no API key.

## Before anything else

Check the binary is there and find out which brain you are talking to:

```bash
brain --version
brain where          # prints the brain that would be used here, and why
```

If `brain --version` fails with "command not found", install it — this is a
single prebuilt binary, not a build, and it needs no toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.ps1 | iex
```

Say what you are about to install before running it; installing software is the
user's call, not yours. Then:

- It lands in `~/.local/bin`, which is not always on `PATH`. If the freshly
  installed `brain` is still not found, call it as `~/.local/bin/brain` for the
  rest of the session and tell the user how to add the directory permanently —
  do not edit their shell profile yourself.
- `BRAIN_INSTALL_DIR=/somewhere/else` if `~/.local/bin` is wrong for this
  machine.
- If the install itself fails, stop and report it. Do not fall back to
  `cargo install`: that needs a Rust toolchain and a C compiler, and takes
  minutes.

`brain where` reporting that nothing exists yet means you must create one before
writing. Do not create one silently in a user's repository — ask first:

```bash
brain init --label "project-name"      # ./.brain/brain.db
brain init --global --label "personal" # the user's global brain
```

**Every command accepts `--json`. Use it.** Parse the JSON rather than the
human-readable lines; the plain output is for people and its shape is not a
contract.

## Recording what you learn

Anything with a subject, a property and a value. There is no schema to declare
and no domain the model prefers:

```bash
brain remember --subject auth --predicate strategy --value "server-side sessions"
brain remember --subject api_gateway --predicate timeout --value 30 --unit s
brain remember --subject checkout_service --predicate owner --value "platform-team"
brain remember --subject release_1_4 --predicate freeze_date --value 2026-08-15
```

- `--subject` is the thing, `--predicate` is the property, `--value` is the
  value. A bare number is stored as a number.
- **If the value names something you could ask another question about, it is
  `--entity`, not `--value`.** See [Relations](#relations) — this is the single
  easiest way to write a fact the brain can never use.
- `--at 2026-01-01` says when it became true in the world. Leave it off for
  "now". Use it whenever you learn something late — a backdated write slots into
  the timeline correctly instead of pretending it just happened.
- `--source` says who claimed it. Pass it. Attribution is what makes a fact
  reviewable later.
- `--locator '{"file":"src/auth/session.rs","lines":"40-52"}'` records *where the
  answer actually lives*. Prefer a locator over pasting a wall of code into a
  fact: the brain is an index, not a warehouse.

The response tells you which of three things happened, and they mean different
things:

| outcome | meaning |
|---|---|
| `created` | new. Nothing was known about this before. |
| `superseded` | it changed. The old value was true, then stopped being. |
| `corrected` | it was never true. The old claim was retracted. |
| `reasserted` | told again. Reinforced, not duplicated. |

If you get `superseded` when you expected `created`, the brain already knew
something. Read the history before assuming your value is the right one.

## Reading

```bash
brain get auth strategy                       # what holds now
brain get auth strategy --as-of 2026-03-01    # what held then
brain history auth strategy                   # the whole trajectory
```

Use `recall` when you do not know the exact subject and predicate — it takes a
natural-language question and fuses five retrieval channels (words, entity names,
meaning, walking the graph, and entities that merely have something in common
with what you asked about):

```bash
brain recall "how does the service authenticate" --json
brain recall "what changed about who owns checkout" --history --json
```

`recall` answers with what is **currently true** by default. `--as-of <when>`
travels in time; `--history` returns closed intervals too. A retracted fact
appears in none of them — it was never true, so replaying it would be a lie.

## Relations

A relation is a fact whose object is another entity, so it gets a timeline for
free — a dependency that moved is a closed interval, not a deleted row.

```bash
brain link checkout_service depends_on payments_db
brain entity payments_db --neighbors --json
```

This is what lets an answer live where the question's words never reach. Asking
"which region does checkout_service data live in" finds `payments_db region
eu-west-1`, one hop away, sharing no word with the question — and reaching an
entity the question never named.

### Connect what you write

**The rule: if the value names something you could ask another question about,
it is an entity.** A class, an owner, a supplier, a service, a category, a
team. Only literals — numbers, dates, flags, one short sentence — are `--value`.

```bash
brain remember --subject voucher_x --predicate is_a --value  voucher_sazonal   # wrong
brain remember --subject voucher_x --predicate is_a --entity voucher_sazonal   # right
brain link voucher_x is_a voucher_sazonal                                      # same thing
```

The wrong one is dangerous precisely because it is not an error. It succeeds, it
reads back correctly, `get` and `history` print it perfectly — and no walk of the
graph can follow it, so the fact is invisible to the one channel that finds
answers a question never named. In one real brain this happened to 59 of 69
`is_a` facts: every voucher knew its class, and no voucher could be reached from
it. Nothing failed for months.

`is_a`, `depends_on`, `part_of`, `owned_by`, `runs_on`, `serves` and anything
shaped like them are always relations. When in doubt, ask whether you would ever
want `brain entity <that value> --neighbors` to return something. If yes, it is
an entity.

The brain learns this for itself: the first time a predicate is recorded with an
entity object, later string values under it are resolved to entities too. So
getting `is_a` right once fixes it going forward. Override with
`brain predicate <name> --relational` or `--relational false`; a declared setting
is never revised.

Check yourself before finishing a session:

```bash
brain lint          # relations stored as strings, entities nothing can reach
brain lint --strict # non-zero exit, for a CI step or a hook
```

If `lint` reports a predicate stored both ways, the facts already written stay as
they are — inference only changes new ones. Fix those explicitly:

```bash
brain predicate is_a --relational      # say what it is
brain repair --relations               # dry run: shows exactly what it would do
brain repair --relations --apply       # link them
```

`repair` changes how a fact is stored, never what it says — the statement comes
out byte-identical and nothing is retracted. If a claim was actually *wrong*,
that is `retract`, not this.

## Correcting yourself

Two different operations. Choosing the wrong one corrupts the history:

- **The value changed** → just `remember` the new one. The old one stays true
  for its period.
- **The value was never right** → `brain retract <fact_id> --reason "..."`.

Get the id from `--json` output, and inspect before retracting:

```bash
brain why 42 --json     # where it came from, what replaced it
```

Retraction deliberately does not reopen whatever the retracted fact closed.
"This was wrong" leaves that period genuinely unknown, which is more honest than
inventing an answer for it.

## Names

If the user calls something by a name the brain does not know, declare it:

```bash
brain alias payments_db "the payments database"
```

A declared alias decides identity — later facts about "the payments database"
land on `payments_db`. There is also `brain recall "..." --learn`, which lets the brain keep the
name a question used when the answer was unambiguous, but that is a guess and
only ever widens search; it never decides where a fact is written.

## When to write a fact

Write when the answer is **stable, specific and worth more than one session**:
a decision and its reason, a config value, an owner, a deadline, a version, a
constraint the user stated, where something lives in the codebase.

Do not write:

- Anything already in the repository or in git history. The brain is for what
  the code does not record.
- Transient state ("the test is currently failing").
- Long prose. One fact, one claim. If it needs a paragraph, store a locator to
  the paragraph.
- Anything the user did not actually assert. A guess recorded as a fact is worse
  than no memory at all, because the next session will trust it.

## Gotchas

- **Identity is exact; search is forgiving.** `André` and `andre` are different
  entities but either question finds either fact. When *writing*, be consistent
  with the spelling already in the brain — check with `brain entity <name>`
  first. Two spellings of one thing means two parallel histories, and that is
  not recoverable.
- **New predicates are single-valued by default.** A second value supersedes the
  first. If a predicate genuinely holds several values at once (tags, members),
  say so: `brain predicate tags --cardinality multi`.
- **A directory with no brain is an error, never a fallback to the global one.**
  If a command fails with "no brain", run `brain where` and read the reason
  rather than guessing which file it wanted.
- **Nothing is deleted.** There is no `brain forget <fact>`. That is the point,
  but it means a fact written carelessly is visible forever in `history`.

## If MCP is available instead

`brain serve` exposes the same operations as MCP tools (`remember`, `link`,
`recall`, `get`, `history`, `entity`, `why`, `retract`, `alias`). When those
tools are connected, prefer them over shelling out — same core, structured
results, no output parsing. Everything above still applies: the tools are the
same operations with the same rules.

## Full command list

```
init  where  stats  lint  remember  link  get  recall  history
entity  why  retract  alias  reindex  predicate  repair
```

`brain <command> --help` for the flags. `--brain <path>`, `--use <name>` and
`--global` choose which brain to talk to.
