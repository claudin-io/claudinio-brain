/**
 * claudinio-brain for OpenClaw.
 *
 * OpenClaw has no command hooks -- plugins are in-process TypeScript, registered
 * through `api.on` -- so, like the OpenCode plugin next door, this is code rather
 * than a config file. What it does is the same: read the project's brain before
 * the model plans, so the agent does not have to decide to ask.
 *
 * Install it with OpenClaw's own command rather than by dropping it somewhere:
 *
 *   openclaw plugins install --link ~/.openclaw/plugins/claudinio-brain
 *
 * and grant it conversation access in openclaw.json, which is a claim on
 * somebody's setup that this project does not make for them:
 *
 *   { "plugins": { "entries": { "claudinio-brain": {
 *       "enabled": true, "hooks": { "allowConversationAccess": true } } } } }
 *
 * `before_prompt_build` is the only hook used, because it is the only one that
 * can add anything: `session_start` fires and its return value is observed
 * rather than read, so there is nowhere to put an introduction. Recall is the
 * half that matters most anyway -- it is the one that runs on every turn.
 *
 * The prompt crosses as `--prompt` rather than on stdin, and the subprocess is
 * spawned with an argument array rather than a shell string. Both for the same
 * reason: a quoting bug in a hook does not raise, it returns nothing, and
 * nothing is indistinguishable from a brain with nothing to say.
 */

import { execFile } from "node:child_process"

/** How long the brain gets before the turn goes on without it. */
const TIMEOUT_MS = 10_000

/** The shortest prompt worth asking about -- matches `hook::MIN_PROMPT_CHARS`. */
const MIN_PROMPT_CHARS = 8

function recall(prompt, cwd) {
  return new Promise((resolve) => {
    execFile(
      "brain",
      // `--format text` because there is no envelope to speak here: this plugin
      // is itself the thing splicing the answer in, so it wants the answer alone.
      ["hook", "recall", "--format", "text", "--prompt", prompt],
      { cwd, timeout: TIMEOUT_MS, maxBuffer: 1 << 20 },
      (err, stdout) => {
        // A memory that can break somebody's turn is worse than no memory. Every
        // failure -- no binary on PATH, no brain in this directory, a timeout --
        // resolves to nothing, which is the rule the shell hooks follow too.
        resolve(err ? "" : String(stdout || "").trim())
      },
    )
  })
}

export default function register(api) {
  api.on("before_prompt_build", async (event, ctx) => {
    try {
      const prompt = event?.cleanedBody ?? event?.prompt
      if (typeof prompt !== "string" || prompt.trim().length < MIN_PROMPT_CHARS) return

      const context = await recall(prompt, ctx?.cwd)
      if (!context) return
      return { prependContext: context }
    } catch {
      // Returning undefined leaves the prompt exactly as it was.
      return
    }
  })
}
