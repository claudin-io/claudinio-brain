"""The same three calls, in the shape each framework asks for them in.

Every adapter here is a plain callable or a factory for one, and **no framework
is imported** -- not at module scope, not lazily, not at all. That is not
laziness about types; it is the honest boundary of what this package can
promise. A framework's callback protocol is a shape, and matching a shape needs
no dependency on the library that defines it. Subclassing its base classes would
need one, would pin a version, and would break on somebody else's upgrade for no
benefit this package could point at.

Where a framework genuinely wants a subclass -- LangChain's `BaseCallbackHandler`,
the OpenAI Agents SDK's `RunHooks` -- `docs/frameworks.md` has the five lines to
write, and they call straight into `core`. Five lines you can read beat an
adapter you have to trust.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from . import core

__all__ = ["langgraph_node", "autogen_hook", "prime_crew_task", "instructions"]


def _last_user_text(state: Any) -> str:
    """The most recent message's text, from whichever shape the state uses.

    LangGraph states carry `messages` as dicts, as LangChain message objects, or
    as anything a user put there. Reading defensively is the point: a state this
    does not recognise means no recall, never an exception in somebody's graph.
    """
    try:
        messages = state["messages"]
        last = messages[-1]
    except Exception:
        return ""
    for get in (lambda: last["content"], lambda: last.content):
        try:
            text = get()
        except Exception:
            continue
        if isinstance(text, str):
            return text
    return ""


def langgraph_node(*, cwd: str | None = None) -> Callable[[Any], dict]:
    """A graph node that adds one system message, or adds nothing.

    Wire it at the entry of the graph::

        builder.add_node("brain", langgraph_node())
        builder.add_edge(START, "brain")

    It returns a bare dict rather than a `SystemMessage`, which LangGraph's
    reducers accept and which keeps this file free of the import.
    """

    def node(state: Any) -> dict:
        found = core.recall(_last_user_text(state), cwd=cwd)
        if not found:
            return {}
        return {"messages": [{"role": "system", "content": found}]}

    return node


def autogen_hook(*, cwd: str | None = None) -> Callable[[str], str]:
    """AutoGen's `process_last_received_message` shape: a string in, a string out.

        agent.register_hook("process_last_received_message", autogen_hook())
    """

    def hook(message: str) -> str:
        return core.prepend(message, cwd=cwd)

    return hook


def prime_crew_task(task: Any, *, cwd: str | None = None) -> Any:
    """Puts what the brain holds in front of a CrewAI task's description.

    Called before `kickoff`, and it touches exactly one attribute -- a task
    carries an expected output, an agent and tools besides, and none of them is
    this function's business.
    """
    description = getattr(task, "description", None)
    if isinstance(description, str):
        found = core.recall(description, cwd=cwd)
        if found:
            task.description = f"{found}\n\n{description}"
    return task


def instructions(prompt: str, *, cwd: str | None = None) -> str:
    """For anything that takes a string of dynamic instructions per run.

    Pydantic AI's `@agent.instructions` and Google ADK's `before_agent_callback`
    both reduce to this.
    """
    return core.recall(prompt, cwd=cwd)
