"""Running `brain` from inside somebody else's agent loop.

This is the recipe in ``docs/frameworks.md`` with a name, and it is deliberately
small: three subcommands and one rule.

The rule is that **nothing here raises**. The Rust hooks are held to it because a
hook is code nobody is watching; a framework callback is held to it for a sharper
reason -- a hook that raises is ignored by its harness, but a callback that
raises stops the turn. So a missing binary, an unreadable brain, a timeout and an
explicit off switch all produce the empty string, and the caller goes on exactly
as it would have without a memory at all.

The other half of that rule is what the empty string *means*. It is "nothing to
say", which is what a brain answers most of the time: no fact matched, or this
directory has no brain, and neither is an error worth telling anybody about.
"""

from __future__ import annotations

import os
import subprocess

__all__ = ["recall", "context", "flush", "prepend", "binary", "enabled"]

#: How long recall gets before the turn goes on without it. Reading a brain is
#: one process and no network -- see the project's README -- so this is a bound
#: on something going wrong rather than on the work.
TIMEOUT = 10.0

#: The introduction reads more of the brain than recall does, and it happens
#: once.
CONTEXT_TIMEOUT = 15.0


def binary() -> str:
    """The `brain` to run.

    `BRAIN_BIN` first, for the case that matters: a virtualenv on a machine
    where `brain` was installed somewhere `PATH` does not reach, which is a
    normal state of affairs rather than a misconfiguration.
    """
    return os.environ.get("BRAIN_BIN") or "brain"


def enabled() -> bool:
    """Whether the hooks should run at all.

    The same `BRAIN_HOOK=off` the shell hooks read, and for the same reason:
    switching a memory off should not require editing the code that installed
    it, which is usually not where the person debugging is looking.
    """
    return os.environ.get("BRAIN_HOOK") != "off"


def _run(what: str, *args: str, cwd: str | None = None, timeout: float) -> str:
    if not enabled():
        return ""
    try:
        done = subprocess.run(
            # An argument list, never a shell string. Somebody's prompt goes
            # across this boundary, and a quoting bug here does not raise -- it
            # returns nothing, which is indistinguishable from a brain with
            # nothing to say.
            [binary(), "hook", what, "--format", "text", *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            cwd=cwd,
        )
    except Exception:
        # Every one of them: no such binary, no permission, a timeout, an OS
        # refusing to fork. The turn is worth more than the memory.
        return ""
    if done.returncode != 0:
        return ""
    return done.stdout.strip()


def recall(prompt: str, *, cwd: str | None = None, timeout: float = TIMEOUT) -> str:
    """What this brain already holds about `prompt`, or "".

    The prompt is passed as `--prompt` rather than on stdin: an argument has no
    API to get wrong, and the alternative is a pipe whose failure mode is
    silence.
    """
    if not isinstance(prompt, str) or not prompt.strip():
        return ""
    return _run("recall", "--prompt", prompt, cwd=cwd, timeout=timeout)


def context(*, cwd: str | None = None, timeout: float = CONTEXT_TIMEOUT) -> str:
    """What this brain is: its label, what it holds, what the last session did.

    Worth exactly once per run. It is counts and vocabulary rather than
    contents, because "you have memory" is not something an agent can act on but
    "this brain holds 312 facts, mostly `owner` and `depends_on`" is.
    """
    return _run("context", cwd=cwd, timeout=timeout)


def flush(*, cwd: str | None = None, timeout: float = TIMEOUT) -> str:
    """The standing ask to write down what this run learned, before it ends.

    A framework has no compaction event to attach this to, so it is yours to
    place -- typically the last turn of a long run.
    """
    return _run("flush", cwd=cwd, timeout=timeout)


def prepend(prompt: str, *, cwd: str | None = None, timeout: float = TIMEOUT) -> str:
    """`prompt` with whatever the brain holds about it in front, or unchanged.

    The one-line form of every recipe in `docs/frameworks.md`.
    """
    found = recall(prompt, cwd=cwd, timeout=timeout)
    return f"{found}\n\n{prompt}" if found else prompt
