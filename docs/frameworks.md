# Frameworks

An agent framework is not a harness. A harness — Claude Code, Codex, Hermes —
runs a loop somebody else wrote and lets you attach commands to its lifecycle,
which is what [`docs/harnesses.md`](harnesses.md) is about. CrewAI, LangChain,
LangGraph, AutoGen, the OpenAI Agents SDK, Google ADK and Pydantic AI are
libraries: *you* write the loop, and the extension points are Python callbacks in
your own process rather than subprocesses a config file can name.

That difference changes who installs the memory, not whether it can be installed.

## Two ways in, and they are not substitutes

**MCP** (`brain serve`) makes the brain a tool the agent can call. Every framework
listed here speaks MCP as a client, and this needs nothing from this page: point
it at `brain serve` and the agent gets twelve tools — `recall`, `get`, `history`,
`which`, `remember`, `why` and the rest.

**A callback that shells out** makes the brain something the agent reads *without
deciding to*. This is the half MCP cannot do. A tool the model has to choose to
call answers the questions somebody already suspected it could answer, and stays
silent about the value it would have corrected — silence being indistinguishable
from having nothing to say.

Use both. The recipes below are the second one.

## The pattern, once

Every recipe is the same three lines with a different callback around them:

```python
import subprocess

def recall(prompt: str) -> str:
    """What this brain already holds about the prompt. Never raises."""
    try:
        out = subprocess.run(
            ["brain", "hook", "recall", "--format", "text", "--prompt", prompt],
            capture_output=True, text=True, timeout=10,
        )
        return out.stdout.strip()
    except Exception:
        return ""
```

Three things are load-bearing, and all three are the same rules the shell hooks
follow:

- **It never raises.** No binary on `PATH`, no brain in this directory, a
  timeout — every one of them returns the empty string and the turn goes on. A
  memory that can break somebody's agent is worse than no memory.
- **The prompt is an argument**, not a shell string. `shell=True` with somebody's
  prompt in it is a quoting bug waiting to be a silent one.
- **`--format text`** because your callback is the thing splicing the answer in,
  so it wants the answer alone rather than a harness's envelope.

`brain hook context --format text` is the other half: the introduction, run once
when the agent starts rather than on every turn.

There is a package that has already written it:

```console
$ pip install claudinio-brain-hooks
```

`recall`, `context`, `flush` and `prepend`, plus the four callback shapes that
can be matched without importing anything — `langgraph_node()`, `autogen_hook()`,
`prime_crew_task()` and `instructions()`. See [`python/`](../python/).

It is a convenience rather than a dependency, and it stops where honesty does:
the frameworks that want a subclass are written out below as five lines each,
because five lines you can read beat an adapter you have to trust.

## Capture does not apply

`brain hook capture` reads a harness's own transcript file, and a framework has
no such file: the conversation is a list of objects in your process. So the
sessions scope stays empty unless you write to it deliberately, which is one
`brain remember --batch -` at the end of a run, or the MCP `remember` tool.

That is a smaller loss than it looks. Capture exists because a harness's
transcript is the only record of a session that nobody chose to write down; when
you own the loop, you already have the messages.

## The recipes

Each is the callback, not the framework. `recall` is the function above.

**LangChain** — a callback handler, on the run's start:

```python
from langchain_core.callbacks import BaseCallbackHandler

class BrainRecall(BaseCallbackHandler):
    def on_chain_start(self, serialized, inputs, **kwargs):
        text = inputs.get("input", "")
        if context := recall(text):
            inputs["input"] = f"{context}\n\n{text}"
```

**LangGraph** — a node at the entry of the graph, which is the framework's own
idiom: state in, state out.

```python
def brain_recall(state):
    last = state["messages"][-1].content
    context = recall(last)
    return {"messages": [SystemMessage(content=context)]} if context else {}

builder.add_node("brain", brain_recall)
builder.add_edge(START, "brain")
```

**CrewAI** — `step_callback` on the crew, or prepend to the task description
before `kickoff`:

```python
task.description = f"{recall(task.description)}\n\n{task.description}".strip()
crew = Crew(agents=[...], tasks=[task])
```

**OpenAI Agents SDK** — `RunHooks`, which is the one framework here with a hook
object shaped like a harness's:

```python
from agents import RunHooks

class BrainHooks(RunHooks):
    async def on_agent_start(self, context, agent):
        if intro := context_text():        # `brain hook context --format text`
            agent.instructions = f"{agent.instructions}\n\n{intro}"
```

**AutoGen** — a `register_hook` on `process_last_received_message`:

```python
agent.register_hook(
    "process_last_received_message",
    lambda message: f"{recall(message)}\n\n{message}".strip(),
)
```

**Google ADK** — `before_agent_callback`, which can return content that replaces
or precedes the turn:

```python
agent = LlmAgent(..., before_agent_callback=lambda ctx: inject(recall(prompt_of(ctx))))
```

**Pydantic AI** — dynamic instructions, evaluated per run:

```python
@agent.instructions
def brain(ctx) -> str:
    return recall(ctx.deps.prompt)
```

**NanoClaw** — a different shape again. It runs the Claude Agent SDK inside
containers and is customised by forking it, so there is no callback to register
from outside. Three things make it work:

1. Mount the `brain` binary and the brain file into the container.
2. Point the container's MCP config at `brain serve`, which is the same
   configuration any MCP client takes.
3. For the read-without-asking half, the Agent SDK's own `PreToolUse` /
   `UserPromptSubmit` hooks are Python functions in your fork — the recipe above,
   registered where the SDK takes hooks.

## What this page is not

None of the above is a verified row in [`docs/harnesses.md`](harnesses.md), and
the distinction is deliberate. Those rows are contracts somebody read and
behaviour somebody checked. These are recipes against APIs that move, written to
be short enough to read before you trust them.

A memory wired in wrong does not fail: it installs cleanly, runs on every turn,
and does nothing — which looks exactly like a brain with nothing to say. So check
that yours answers before you rely on it:

```console
$ brain hook recall --format text --prompt "something this brain should know"
```

If that prints nothing at your terminal, no callback is going to make it print
something.
