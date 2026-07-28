---
name: claudinio-brain
description: Give the agent durable, time-aware memory backed by the `brain` CLI — record facts, decisions, config values and relations, then recall what is true now or what was true at any past instant. Use when the user says remember this, what did we decide, what is the current value, what was it before, what changed, why did it change, or when a fact learned in one session must survive into the next. Also use before answering from assumption about a project-specific value (a port, an owner, a price, a deadline) that the brain may already hold.
license: MIT
compatibility: Requires the `brain` binary on PATH (cargo install --git https://github.com/claudin-io/claudinio-brain). Local only; makes no network requests.
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

```bash
brain remember --subject produto_a --predicate preco --value 20
brain remember --subject api_gateway --predicate timeout --value 30 --unit s
brain remember --subject deploy --predicate responsavel --value "maria"
```

- `--subject` is the thing, `--predicate` is the property, `--value` is the
  value. A bare number is stored as a number.
- `--at 2026-01-01` says when it became true in the world. Leave it off for
  "now". Use it whenever you learn something late — a backdated write slots into
  the timeline correctly instead of pretending it just happened.
- `--source` says who claimed it. Pass it. Attribution is what makes a fact
  reviewable later.
- `--locator '{"file":"src/pricing.rs","lines":"40-52"}'` records *where the
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
brain get produto_a preco                       # what holds now
brain get produto_a preco --as-of 2026-03-01    # what held then
brain history produto_a preco                   # the whole trajectory
```

Use `recall` when you do not know the exact subject and predicate — it takes a
natural-language question and fuses four retrieval channels (words, entity
names, meaning, and walking the graph):

```bash
brain recall "quanto custa o produto_a" --json
brain recall "what changed about the deploy owner" --history --json
```

`recall` answers with what is **currently true** by default. `--as-of <when>`
travels in time; `--history` returns closed intervals too. A retracted fact
appears in none of them — it was never true, so replaying it would be a lie.

## Relations

A relation is a fact whose object is another entity, so it gets a timeline for
free — a supplier that changed is a closed interval, not a deleted row.

```bash
brain link produto_a fornecido_por acme
brain entity acme --neighbors --json
```

This is what lets an answer live where the question's words never reach. Asking
"which country does produto_a come from" finds `acme pais Chile`, one hop away,
sharing no word with the question.

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
brain alias acme "ACME Corp"
```

A declared alias decides identity — later facts about "ACME Corp" land on
`acme`. There is also `brain recall "..." --learn`, which lets the brain keep the
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

- **Identity is exact; search is forgiving.** `preço` and `preco` are different
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

## Full command list

```
init  where  stats  remember  link  get  recall  history
entity  why  retract  alias  reindex  predicate
```

`brain <command> --help` for the flags. `--brain <path>`, `--use <name>` and
`--global` choose which brain to talk to.
