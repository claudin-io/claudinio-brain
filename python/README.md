# claudinio-brain-hooks

Read a project's [brain](https://github.com/claudin-io/claudinio-brain) from
inside an agent loop you wrote yourself.

```console
$ pip install claudinio-brain-hooks
```

```python
from claudinio_brain_hooks import prepend

prompt = prepend(prompt)   # what the brain already holds, in front of what was asked
```

It needs the `brain` binary on `PATH` (or named in `BRAIN_BIN`) and nothing else.
There are no dependencies, and there is no configuration.

## What it is for

`brain` reaches a *harness* — Claude Code, Codex, Hermes — through lifecycle
hooks it installs into that harness's own config. A framework has no lifecycle to
install into, because you write the loop. So it reaches one of these instead: the
same three subcommands, called from your own callback.

| call | what it answers |
|---|---|
| `recall(prompt)` | what the brain already holds about this prompt, with dates |
| `context()` | what this brain is, and what the last session here did |
| `flush()` | the standing ask to write down what this run learned |
| `prepend(prompt)` | `recall` plus the prompt, or the prompt unchanged |

`adapters` has the four shapes that need no import to match: `langgraph_node()`,
`autogen_hook()`, `prime_crew_task(task)` and `instructions(prompt)`.

## What it will not do

**It never raises.** A missing binary, a brain-less directory, a timeout, a
non-zero exit, `BRAIN_HOOK=off` — every one of them returns the empty string and
your turn goes on exactly as it would have without a memory. A hook that raises
is ignored by its harness; a callback that raises stops the turn, which is why
this rule is stricter here than in the binary.

**It imports no framework.** Not at module scope, not lazily. A callback protocol
is a shape, and matching a shape needs no dependency on the library that defines
it — so nothing here pins a version, and nothing here breaks on somebody else's
upgrade. Where a framework genuinely wants a subclass (LangChain's
`BaseCallbackHandler`, the OpenAI Agents SDK's `RunHooks`),
[docs/frameworks.md](../docs/frameworks.md) has the five lines to write. Five
lines you can read beat an adapter you have to trust.

**It does not capture sessions.** `brain hook capture` reads a harness's own
transcript file, and a framework has no such file. Write what a run learned
deliberately, with `brain remember --batch -` or the MCP `remember` tool.

## Tests

```console
$ cd python && python3 -m pytest
```

They run against a `brain` this repository writes into a temporary directory, so
they need no brain, no binary and no network.

## Publishing

Not published yet. The workflow in
[`.github/workflows/python.yml`](../.github/workflows/python.yml) builds and
tests it; releasing to PyPI needs the project created there and trusted
publishing configured, which is a step for whoever owns the account.
