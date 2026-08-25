/**
 * claudinio-brain for OpenCode and Kilo Code.
 *
 * Both load plugins the same way, and Kilo will read an `opencode.json` besides,
 * so this one file serves both. Install by copying it to:
 *
 *   .opencode/plugins/          or  ~/.config/opencode/plugins/
 *   .kilo/plugin/               or  ~/.config/kilo/plugin/
 *
 * Neither of them has command hooks, so unlike every other harness here this is
 * code rather than a config file. What it does is the same thing: read the
 * project's brain on every prompt, so the agent does not have to decide to.
 *
 * `chat.message` returns void, so injection means mutating what is already in
 * `output.parts`. This prepends onto the first text part rather than building a
 * new one -- a Part constructed here would be this file guessing at a shape it
 * does not own, and the guess would break silently the first time that shape
 * changed.
 *
 * The prompt goes across as `--prompt` rather than on stdin for the same reason.
 * Piping into a subprocess means using whichever shell API this runtime happens
 * to expose, and getting that subtly wrong yields a plugin that returns nothing
 * rather than one that errors -- which is indistinguishable from a brain with
 * nothing to say. An argument has no API to get wrong.
 */

export const ClaudinioBrain = async ({ $, directory }) => ({
  "chat.message": async (_input, output) => {
    const parts = output?.parts
    if (!Array.isArray(parts)) return

    const first = parts.find((p) => p?.type === "text" && typeof p.text === "string")
    if (!first || first.text.trim().length < 8) return

    let context
    try {
      // `--format text` because there is no envelope to speak here: this plugin is
      // itself the thing splicing the answer in, so it wants the answer alone.
      context = (
        await $`brain hook recall --format text --prompt ${first.text}`.cwd(directory).quiet().text()
      ).trim()
    } catch {
      // A memory that can break somebody's prompt is worse than no memory. Every
      // failure here -- no binary on PATH, no brain in this directory -- leaves
      // the prompt untouched, which is the rule the shell hooks follow too.
      return
    }

    if (!context) return
    first.text = `${context}\n\n${first.text}`
  },
})

export default { id: "claudinio-brain", server: ClaudinioBrain }
