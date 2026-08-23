# The plugin

Three lifecycle hooks that read the brain so nobody has to remember to. The
argument for them is in the README (*Without being asked*); this is how they are
installed, what they cost, and how to turn each one off.

## Install

```
/plugin marketplace add claudin-io/claudinio-brain
/plugin install claudinio-brain@claudin-io
```

That brings the hooks and the [skill](../skills/claudinio-brain/SKILL.md). It
does **not** bring the binary: the plugin is a few files in a git repository and
`brain` is a 14 MB download, so they are installed separately.

```bash
curl -fsSL https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.sh | sh
```

Until the binary is on `PATH` (or at `~/.local/bin/brain`, which the installer
uses and which is not on the `PATH` of every shell a harness spawns), every hook
prints `{}` and stays silent. That is the intended state, not a broken one — the
plugin is enabled once and then spends most of its life in projects that have no
brain at all.

## What each hook does

| event | subcommand | injects |
|---|---|---|
| `SessionStart` | `brain hook context` | the brain's label and path, what it holds, the predicates it has learned, and how to ask it things |
| `UserPromptSubmit` | `brain hook recall` | up to five facts the brain already holds about the prompt, each with the date it became true |
| `PreCompact` | `brain hook flush` | a request to record anything worth more than one session, as a single `remember --batch` |

Every one of them reads. None of them writes. What a session learned still
becomes a fact through a deliberate `remember` the agent runs and the user can
see — a hook that wrote on its own would turn the brain into a transcript log,
and the whole point of a fact is that somebody decided it was one.

## What it costs

The recall hook is a local process against a local SQLite file: no network, no
model, no server. It spends its budget in context rather than in latency —
`INJECT_LIMIT` is five statements, roughly one screen, on every prompt. Prompts
under eight characters are skipped: "ok" names nothing, and the ranking channels
would answer anyway with a confident irrelevance attached to the user's own
words.

## Turning it off

```bash
BRAIN_HOOK=off                 # in the environment: all three hooks answer {}
BRAIN_HOOK_NOT_SCOPE=todo      # ...or keep one namespace out of what is injected
```

A brain that holds a task list holds facts that are true, current, and beside the
point on every prompt that is not about them. `--not-scope` is the existing answer
to that; a hook takes no flags, so it reads the same setting from the
environment.

The off switch is an environment variable on purpose. The settings file that
installed a hook is usually not where the person debugging one is looking, and
`/plugin disable` is a bigger hammer than "not on this machine, today".

To keep some and not others, wire them by hand instead of installing the plugin.
`brain hook <what>` reads the harness's JSON on stdin and writes the harness's
JSON on stdout, so it works anywhere that contract holds:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "brain", "args": ["hook", "recall"], "timeout": 10 }
        ]
      }
    ]
  }
}
```

The event named in the answer is read from the input rather than assumed, so
`flush` attached to `SessionEnd` says `SessionEnd`. That is what makes one
subcommand safe to attach to more than one event.

## Windows

The bundled hooks go through `hooks/brain-hook.sh`, which is POSIX `sh`. On
Windows, wire `brain hook <what>` directly as shown above — the binary is the
same, only the wrapper is not.

## Why the MCP server is not bundled

`brain serve` resolves its brain once, at startup, and a plugin's MCP servers
start in every project. In the projects that have no brain — most of them — that
is a server that fails to start and an error in `/mcp` for a tool nobody asked
for. So it is opt-in, per project, where somebody has already decided a brain
belongs:

```bash
claude mcp add brain -- brain serve
```

See the README's *MCP* section for what the tool surface is.
