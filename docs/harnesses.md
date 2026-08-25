# Harnesses

`brain` reaches an agent two ways, and they are not substitutes.

**MCP** (`brain serve`) makes the brain a tool the agent can call. Any MCP client
works, today, with no configuration in this repository — Zed, VS Code Copilot,
Claude Desktop and everything else that speaks the protocol.

**Hooks** (`brain hook`) make the brain something the agent reads *without
deciding to*. That is the whole point: a memory an agent has to choose to consult
answers the questions somebody already suspected it could answer. Hooks need
per-harness configuration, because every harness invented its own.

## What is verified here

The table says what was checked against a published schema or the harness's own
source, not what is likely to work. Where a harness cannot do something, the row
says so instead of leaving it out.

| harness | introduce | recall (per prompt) | flush | how |
|---|---|---|---|---|
| **Claude Code** | ✅ | ✅ | ✅ | [`hooks/hooks.json`](../hooks/hooks.json), or the plugin |
| **Cline** | ✅ | ✅ | ✅ | [`hooks/cline/`](../hooks/cline/) |
| **Codex** | ✅ | ✅ | ❌ *see below* | [`hooks/codex/hooks.json`](../hooks/codex/hooks.json) |
| **Gemini CLI** | ⚠️ *see below* | ✅ | ❌ | [`hooks/gemini/settings.json`](../hooks/gemini/settings.json) |
| **OpenCode** | ❌ | ✅ | ❌ | [`hooks/opencode/brain.js`](../hooks/opencode/brain.js) |
| **Kilo Code** | ❌ | ✅ | ❌ | the same plugin |
| **Cursor** | ✅ | ❌ *see below* | ❌ | [`hooks/cursor/hooks.json`](../hooks/cursor/hooks.json) |
| **Augment** | ✅ | ❌ *see below* | ❌ | [`hooks/augment/settings.json`](../hooks/augment/settings.json) |
| anything else | — | — | — | `--format text`, wired by hand |

### Codex

Codex implemented Claude Code's hook wire format deliberately — its engine is
literally named `ClaudeHooksEngine` — so the default `--format claude` is already
its format. Input arrives as `snake_case` JSON with `prompt` and
`hook_event_name`; output is read back as `hookSpecificOutput.additionalContext`.

Hooks are opt-in. In `~/.codex/config.toml`:

```toml
[features]
codex_hooks = true
```

Then copy `hooks/codex/hooks.json` to `~/.codex/hooks.json` and replace the
placeholder path.

**Why no flush.** `PreCompactCommandOutputWire` in the Codex source carries only
the universal fields and is `deny_unknown_fields`, so a `hookSpecificOutput` sent
on `PreCompact` is rejected rather than ignored. Wiring flush there would make
Codex complain on every compaction and inject nothing. `SessionStart` and
`UserPromptSubmit` both accept context, and those are what the config uses.

### Gemini CLI

Same `hookSpecificOutput.additionalContext` shape, configured under `hooks` in
`settings.json`.

**The warning on SessionStart.** Gemini CLI has an open upstream issue reporting
that `SessionStart` does not actually inject `additionalContext`
([google-gemini/gemini-cli#15413](https://github.com/google-gemini/gemini-cli/issues/15413)).
The row is left in because the schema accepts it and the behaviour may already
have been fixed in the version you are running; `UserPromptSubmit` is the one
that carries the weight either way.

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
