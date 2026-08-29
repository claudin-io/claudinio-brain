"""Durable, time-aware memory for an agent loop you wrote yourself.

`brain` reaches a *harness* through lifecycle hooks it installs into. A
framework has no lifecycle to install into -- you write the loop -- so it reaches
one of these instead: the same three subcommands, called from your own callback.

    from claudinio_brain_hooks import prepend

    prompt = prepend(prompt)   # what the brain holds, in front of what was asked

See `docs/frameworks.md` in the repository for the recipe each framework wants,
and `brain --help` for the memory itself. This package needs the `brain` binary
on `PATH` (or named in `BRAIN_BIN`) and nothing else.
"""

from .core import binary, context, enabled, flush, prepend, recall

__all__ = ["recall", "context", "flush", "prepend", "binary", "enabled"]
__version__ = "0.1.0"
