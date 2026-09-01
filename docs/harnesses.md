# Harnesses

`brain` reaches an agent two ways, and they are not substitutes.

**MCP** (`brain serve`) makes the brain a tool the agent can call. Any MCP client
works, today, with no configuration in this repository — Zed, VS Code Copilot,
Claude Desktop and everything else that speaks the protocol.

**Hooks** (`brain hook`) make the brain something the agent reads *without
deciding to*, and something that remembers a session *without being asked to*.
That is the whole point: a memory an agent has to choose to consult answers the
questions somebody already suspected it could answer, and a session nobody wrote
down is a session the next one starts without. Hooks need per-harness
configuration, because every harness invented its own.

There are four, and the fourth is different from the other three:

| subcommand | when | what it does |
|---|---|---|
| `hook context` | session start | says what this brain is, and what the last session did |
| `hook recall` | every prompt | answers the prompt from what the brain already holds |
| `hook flush` | before compaction | asks the model to write down what the session *learned* |
| `hook capture` | end of turn, end of session | records what the session *did*, from the transcript, with no model |

The first three read and never write. `capture` writes, and what makes that safe
is that it only ever writes about the session itself — subject `session/<id>`,
scope `sessions`, six predicates (`worked_on`, `edited`, `ran`, `branch`,
`harness`, `concluded`) — so it cannot contradict a value a person recorded. It
never fails, never blocks and prints nothing, exactly like the other three. Run
it by hand against a transcript with `brain hook capture --transcript <path>
--dry-run`.

`edited` counts a file however the session reached it. An agent may be configured
to prefer the shell for file work, and then a session that changed nine files
makes no edit-tool call at all — so a redirection, a `tee`, a `sed -i`, a
`git rm`/`mv`, and an interpreter heredoc that opens a path for writing are all
read as edits, and joined onto the transcript's own `cwd` so the same file is one
row however it was named — and after any `cd` the chain does first, so work in a
clone is not attributed to the directory the harness was launched from. Reading
is still not changing: a `grep`, a `cat`, a `>` into `/tmp` and an `open()`
without a write mode record nothing.

A heredoc body is read as what the line opening it said it was: a program after
`python3 - <<'PY'`, a document after `cat > notes.md <<'EOF'`. The document's
prose can quote a command without claiming it ran. The limit worth knowing is
the one case that cannot be told apart from the text: a session whose work *is*
source code about shell commands — a test fixture full of `open(p, 'w')`, fed to
an interpreter — can still name a file it only ever quoted.

`brain hook context` reads those back at the start of the next session, so an
agent is told what it was doing last time rather than having to think to go
looking. `--not-scope sessions`, or `BRAIN_HOOK_NOT_SCOPE=sessions`, leaves them
out of per-prompt recall if you would rather they stayed out of the way.

## Installing

```console
$ brain hook install claude-code
created /home/you/.claude/settings.json
claude-code is wired up

still to do:
The plugin is the maintained path here (`/plugin install claudinio-brain@claudin-io`);
this writes the same hooks into settings.json by hand instead.
```

`--project` scopes it to this directory instead of every project; `--dry-run`
prints what it would write and writes nothing.

It writes the absolute path of the running binary, so there is no placeholder to
forget. It **merges** into whatever is already in the file — other people's hooks,
other events, unrelated keys all survive — and it replaces its own previous entry
rather than adding a second one, so installing twice leaves one hook. A config
file it cannot parse is refused rather than overwritten.

What it will not do is switch on a harness's feature flags. Those are printed as
"still to do" instead: turning on somebody's settings is a larger claim on their
machine than writing the file they asked for, and a step you perform yourself is
a step you know happened.

The files under `hooks/` are the same configuration for hand-installing, with an
`/ABSOLUTE/PATH/TO/brain` placeholder to replace.

### Which brain the hooks answer from

By default, the brain of whatever directory the session is in — that is the
right wiring for a per-project brain, and it is why installing once, globally,
works for every checkout. To wire hooks that answer from somewhere else, pass
the same selectors every other command takes:

```console
$ brain hook install augment --global        # the global brain
$ brain hook install codex --use work        # a named brain from the catalogue
$ brain hook install gemini --brain /path/to/brain.db
```

The selector is written into every command the install produces, and a global
wiring and a local one are **different wirings**: installing one does not
replace the other, so a harness with only session-start injection (Augment,
Cursor) can carry both — the project's brain and the global one, each
introduced at start. Reinstalling replaces only the entry with the same
selector. This holds for hand-written entries too: a local install leaves a
hand-written `--global` entry exactly where it was.

The three plugin-file harnesses (OpenCode, Kilo Code, OpenClaw) run `brain`
bare from PATH inside a bundled file, so there is nowhere to write a selector;
asking for one there is refused rather than silently ignored.

`hooks/brain-hook.sh` forwards the same flags: the first positional is the
event, the second is the format, and `--global`, `--use <name>`, `--brain
<path>` pass through wherever they appear. It used to read its second argument
as the format unconditionally, which turned `brain-hook.sh context --global`
into `brain hook context --format --global` — an error the wrapper swallows by
design, so the global brain went silently uninjected. The contract is pinned by
`tests/step26_wrapper.rs`.

## What is verified here

The table says what was checked against a published schema or the harness's own
source, not what is likely to work. Where a harness cannot do something, the row
says so instead of leaving it out.

| harness | introduce | recall (per prompt) | flush | capture | how |
|---|---|---|---|---|---|
| **Claude Code** | ✅ | ✅ | ✅ | ✅ *`Stop` + `SessionEnd`* | [`hooks/hooks.json`](../hooks/hooks.json), or the plugin |
| **Cline** | ✅ | ✅ | ✅ | ❌ *see below* | [`hooks/cline/`](../hooks/cline/) |
| **Codex** | ✅ | ✅ | ✅ | ⏳ *see below* | [`hooks/codex/hooks.json`](../hooks/codex/hooks.json) |
| **Gemini CLI** | ⚠️ *see below* | ✅ *on `BeforeAgent`* | ❌ | ⏳ *see below* | [`hooks/gemini/settings.json`](../hooks/gemini/settings.json) |
| **Hermes** | ✅ *first turn only* | ✅ *on `pre_llm_call`* | ❌ | ⏳ *see below* | [`hooks/hermes/config.yaml`](../hooks/hermes/config.yaml) |
| **OpenCode** | ❌ | ✅ | ❌ | ❌ | [`hooks/opencode/brain.js`](../hooks/opencode/brain.js) |
| **OpenClaw** | ❌ *see below* | ✅ *on `before_prompt_build`* | ❌ | ❌ | [`hooks/openclaw/`](../hooks/openclaw/) |
| **Kilo Code** | ❌ | ✅ | ❌ | ❌ | the same plugin |
| **Cursor** | ✅ | ❌ *see below* | ❌ | ❌ | [`hooks/cursor/hooks.json`](../hooks/cursor/hooks.json) |
| **Augment** | ✅ | ❌ *see below* | ❌ | ⏳ *see below* | [`hooks/augment/settings.json`](../hooks/augment/settings.json) |
| anything else | — | — | — | — | `--format text`, wired by hand |

⏳ means the events exist and the transcript format has not been verified here.
Capture reads a harness's own transcript file, and a dialect nobody checked is
exactly the quiet failure the rest of this page exists to avoid — so those rows
stay empty until somebody has read the file.

### Claude Code

All four, and capture is wired to **both** `Stop` and `SessionEnd`.

`Stop` fires when a turn finishes, so the record survives a session that is
killed or crashes and never reaches `SessionEnd`. It is installed with
`"async": true`: it runs between the user's turns, and a hook that makes somebody
wait is a hook they uninstall.

`SessionEnd` is the last and most complete look at the transcript. Its `timeout`
is load-bearing rather than decorative — hooks on that event share a second and a
half unless one of them asks for longer, and a hook killed on its timeout has its
work discarded — so it is installed with `"timeout": 60`.

`SessionStart` is installed with `--format text` rather than the JSON envelope.
Claude Code guarantees that a session-start hook's plain stdout reaches the
context; whether it also unwraps `additionalContext` there is no longer
documented, and an envelope that stops being unwrapped does not fail — it arrives
as its own source code, which is worse than not arriving.

### Codex

Codex implemented Claude Code's hook wire format deliberately — its engine is
literally named `ClaudeHooksEngine` — so the default `--format claude` is already
its format. Input arrives as `snake_case` JSON with `prompt` and
`hook_event_name`; output is read back as `hookSpecificOutput.additionalContext`.

Hooks are on by default now. They used to be gated behind
`[features] codex_hooks = true` in `~/.codex/config.toml`, and the installer used
to print that as a "still to do"; it no longer does, because a step that has no
effect costs more than saying nothing — somebody performs it, sees nothing
change, and stops trusting the rest of the message. `[features] hooks = false`
turns them off.

Copy `hooks/codex/hooks.json` to `~/.codex/hooks.json` and replace the
placeholder path.

**Flush works now.** It used to be impossible: `PreCompactCommandOutputWire`
carried only the universal fields and was `deny_unknown_fields`, so a
`hookSpecificOutput` sent on `PreCompact` was rejected rather than ignored. Codex
documents the shared output contract on `PreCompact` today, and the config wires
flush there.

**Capture is not wired yet.** Codex has `Stop` (with `last_assistant_message`)
and `SessionEnd`, and passes a `transcript_path`. What has not been checked here
is what that file contains — capture parses a transcript rather than a hook
payload, and a dialect nobody read is a hook that installs cleanly and records
nothing.

### Gemini CLI

Same `hookSpecificOutput.additionalContext` shape, configured under `hooks` in
`~/.gemini/settings.json` or `<project>/.gemini/settings.json`.

**Its events are its own.** Gemini has no `UserPromptSubmit`. The event that
fires after a prompt is submitted and before the agent plans — the one place it
takes context for a turn — is **`BeforeAgent`**, and that is what recall attaches
to. Its full lifecycle set is `BeforeTool`, `AfterTool`, `BeforeAgent`,
`AfterAgent`, `BeforeModel`, `BeforeToolSelection`, `AfterModel`, `SessionStart`,
`SessionEnd`, `Notification`, `PreCompress`.

**Timeouts are milliseconds here**, where Claude Code and Codex count seconds.
Same field name, three orders of magnitude apart; a `15` copied across from
another harness is a hook that always times out.

**No flush.** `PreCompress` is advisory and returns only `systemMessage`, which
is shown to the user rather than given to the model.

**Capture is not wired yet.** `SessionEnd` here is non-blocking, ignores
flow-control fields and injects nothing — which is precisely the shape a capture
hook wants, since it writes to the brain rather than to the session. Gemini also
passes `transcript_path` on *every* event. The format of that file has not been
read here, so the row stays empty rather than guessing.

**The warning on SessionStart.** There is an open upstream issue reporting that
`SessionStart` does not actually inject `additionalContext`
([google-gemini/gemini-cli#15413](https://github.com/google-gemini/gemini-cli/issues/15413)).
The row is left in because the schema accepts it and it may be fixed in the
version you are running; `BeforeAgent` carries the weight either way.

### Hermes

Hermes takes shell hooks, declared under `hooks:` in `~/.hermes/config.yaml`,
with the same stdin contract everyone else here uses: JSON on stdin carrying
`hook_event_name`, JSON back on stdout. It also accepts Claude Code's
`decision`/`reason` for blocking a tool call, which nothing here emits — that is
a gate, and a memory that could veto a tool call would be a different tool.

**One event injects.** `pre_llm_call` reads a flat `context` back and appends it
to the current turn's user message. Everything else observes: `on_session_start`
fires, and its return value is documented as ignored. So there is nowhere to
attach an introduction — unless the introduction attaches to the per-turn event
and works out for itself which turn is the first, which is what it does.
`extra.is_first_turn` is in the payload and states it outright, so `hook context`
returns the introduction on the first turn and nothing on every turn after. That
is the same judgement `source: resume` already makes on Claude Code, one harness
over: not every prompt that *can* carry an introduction is a session starting.

**The prompt is `user_message`**, at the top level, where the other harnesses put
`prompt`. One field name in one schema, and getting it wrong is free of any
visible symptom: the hook installs, runs on every turn and answers `{}` forever.

**Its config is YAML, and the file is not ours.** Models, gateway and hooks all
live in `~/.hermes/config.yaml`, so the installer merges into it, refuses what it
cannot parse, and leaves every key and every hook it did not write exactly where
it found them — including the `matcher` and `fail_closed` fields it never writes
itself. What does not survive a parse-and-reserialise is comments, and the
installer says so rather than letting somebody discover it.

**`--project` is refused.** Hermes documents no project-level config, so a
`<project>/.hermes/config.yaml` would be written successfully and read by nobody.

**Consent is a gate, and a gate is a silence.** Hermes asks before it runs a hook
it has not seen, so the first session after installing shows a prompt and the
hooks do nothing until it is answered. `hooks_auto_accept: true` in the same
file, or `HERMES_ACCEPT_HOOKS=1`, skips it.

**No flush.** Hermes has no pre-compaction event to attach one to.

**Capture is not wired yet.** `on_session_end` exists, and `subagent_stop`
besides. What has not been read here is the transcript those point at — the same
reason Codex, Gemini and Augment have an hourglass in that column.

### OpenClaw

No command hooks at all: OpenClaw's plugins are in-process TypeScript, registered
through `api.on(...)` from a plugin directory with a manifest. So this is the
second entry here that is code rather than configuration, and it works the way
the OpenCode one does — the prompt crosses to `brain` as `--prompt`, the
subprocess is spawned from an argument array rather than a shell string, and
every failure path leaves the turn exactly as it was.

`before_prompt_build` is the only hook used, because it is the only one that can
add anything: it returns `prependContext` (among `appendContext`, `systemPrompt`
and others) and gets `event.cleanedBody` or `event.prompt` to work from.
`session_start` and `session_end` are classified as observers there too, so there
is no introduction and no flush.

**Two steps stay yours.** OpenClaw adopts a plugin through its own command rather
than by finding a directory, and it gates conversation hooks behind a flag, so
`brain hook install openclaw` writes the three files and then prints both:

```console
openclaw plugins install --link ~/.openclaw/plugins/claudinio-brain
```

```json
{"plugins": {"entries": {"claudinio-brain": {
  "enabled": true, "hooks": {"allowConversationAccess": true}}}}}
```

Like OpenCode and Kilo, this one has not been run end to end — no OpenClaw
install was available — and the contract it is written against was read from the
published hook reference.

### Cursor

Cursor's `beforeSubmitPrompt` is a **gate**, not an injector: it can permit or
deny a prompt and returns `{"continue": bool, "user_message": string}`. There is
no field on it for adding context, so per-prompt recall — the thing hooks exist
for — cannot be done on Cursor at all. Only `sessionStart` injects, through a
flat `additional_context`, which is what `--format cursor` emits.

So Cursor gets the introduction and nothing else. Pair it with MCP if you want
the brain answerable mid-session.

### Cline

The second harness after Claude Code where all three answers land. Cline finds
hooks **by filename** — no config file — so the scripts in `hooks/cline/` must
keep their names and stay executable. Copy them *and* `hooks/brain-hook.sh` into
`.clinerules/hooks/` (this workspace) or `~/Documents/Cline/Hooks/` (all of them),
then tick "Enable Hooks" in Cline's Feature Settings. macOS and Linux only, which
is Cline's limitation rather than this one.

**No capture.** Cline's three events carry the prompt and not a path to a
transcript, and capture reads a transcript. There is nothing to point it at.

Two details differ from everyone else, and both are handled by `--format cline`:
the prompt arrives nested at `userPromptSubmit.prompt` rather than at the top
level, and the reply carries `cancel` alongside `contextModification`. `cancel` is
always `false` here and is always *stated* — it decides whether the user's prompt
happens at all, and a memory that could veto a keystroke by leaving a field
undefined would be a different and much worse tool.

### OpenCode and Kilo Code

Neither has command hooks. Both load JavaScript plugins with the same signature,
and Kilo reads an `opencode.json` besides, so one file serves both:
[`hooks/opencode/brain.js`](../hooks/opencode/brain.js) → `.opencode/plugins/` or
`.kilo/plugin/` (or the `~/.config/…` equivalents).

The plugin hooks `chat.message`, which returns `void` — injection therefore means
mutating `output.parts`, and it prepends onto the first text part rather than
constructing a new one. A `Part` built here would be this project guessing at a
shape it does not own.

Only recall. Neither exposes a session-start or pre-compaction hook that can add
context, so there is nothing to attach the introduction or the flush to.
Kilo has an [open request](https://github.com/Kilo-Org/kilocode/issues/5827) for
session lifecycle hooks; if it lands, the other two become possible.

OpenCode's plugin API has grown `session.created`, `session.idle`,
`session.compacted` and an experimental `session.compacting` since this plugin
was written, which are the shapes introduce, capture and flush would attach to.
None of them is wired here yet.

This is the one entry here that is code rather than configuration, and it is the
one that has not been run end to end — no OpenCode or Kilo install was available.
The contract it is written against was read from the plugin type definitions in
OpenCode's source, and the prompt crosses as `--prompt` precisely so that no
untested shell-API guess sits on the path.

### Augment

Auggie reads `hookSpecificOutput.additionalContext`, the same envelope Claude
Code, Codex and Gemini CLI use, from a `hooks` block in
`~/.augment/settings.json` or `<workspace>/.augment/settings.json`.

It has `SessionStart`, `PreToolUse`, `PostToolUse`, `Stop` and `SessionEnd` — and
**no `UserPromptSubmit`**. So, like Cursor, Augment gets the introduction and
nothing per-prompt. `SessionEnd` cannot inject, so there is no flush either.

`Stop` and `SessionEnd` both exist, so capture is possible in principle; as with
Codex and Gemini, what is missing is somebody having read the transcript file it
would parse.

Because Augment only injects at `SessionStart`, the useful setup for someone
with both a project brain and a global one is two entries on that event:

```console
$ brain hook install augment
$ brain hook install augment --global
```

## Everything else

`--format text` writes the context and nothing else, and a prompt handed to
`brain hook` on stdin as plain text is read as a prompt rather than ignored. That
is enough to wire `brain` into a harness by hand without anybody here having
guessed at its schema.

It is deliberately not the same as a supported row above. A config nobody has
checked against a real contract is worse than no config: hooks never fail and
never explain, so a wrong one installs cleanly, runs on every prompt, and does
nothing — which looks exactly like a brain with nothing to say.

If you want a harness added, the three things needed are its event names, the
shape it puts on stdin, and the exact field it reads context back from.

## Agents and frameworks with no hook surface

Some agents have no lifecycle hooks to install into at all. NanoClaw runs the
Claude Agent SDK inside containers and is customised by forking it; CrewAI,
LangChain, LangGraph, AutoGen, the OpenAI Agents SDK, Google ADK and Pydantic AI
are libraries whose extension points are in-process Python callbacks, not
subprocesses a config file can name.

They are not out of reach — they are reached differently, through MCP and through
a callback that shells out. [docs/frameworks.md](frameworks.md) has a recipe for
each, and says plainly which parts are wired by hand rather than verified here.
