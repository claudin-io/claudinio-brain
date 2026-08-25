#!/bin/sh
# Runs `brain hook <what>` for a Claude Code lifecycle event, and is careful about
# the two ways that can go wrong in somebody else's session.
#
# 1. The plugin is installed and the binary is not. That is an ordinary state --
#    the plugin is a file in a git repository, the binary is 14 MB somebody has to
#    fetch -- and it must be silent, not an error on every prompt. `~/.local/bin`
#    is checked by hand because that is where the installer puts it and it is not
#    on the PATH of every shell a harness spawns.
#
# 2. The binary is there and something goes wrong anyway. `brain hook` is written
#    never to fail, but "never" is a claim about code, and this is the place where
#    that claim being wrong costs a user something. Anything other than JSON on
#    stdout becomes `{}`.
#
# Empty output means the same as `{}` and is printed as such: a hook that exits 0
# with nothing at all is a hook the harness has to guess about.
set -u

# $2 is the output envelope, and defaults to the one Claude Code reads. Codex and
# Gemini CLI read the same shape, so they pass nothing either; `cursor` and `text`
# are the two that differ. See docs/harnesses.md.
FORMAT=${2:-claude}

# What "nothing to say" looks like, which is not the same sentence in every
# envelope: a harness parsing JSON needs an empty object, and one splicing stdout
# into a prompt needs an empty *file*. Printing `{}` into a prompt would be the
# hook adding noise on the one path where it had nothing to add.
nothing() {
  case "$FORMAT" in
    # An empty *file*, because this one gets spliced into a prompt and `{}` there
    # would be the hook adding noise on the one path where it had nothing to add.
    text) ;;
    # Cline reads `cancel` to decide whether the user's prompt happens at all.
    # Leaving it out and hoping the default is false is not a bet worth taking on
    # somebody else's keystroke.
    cline) printf '{"cancel":false}\n' ;;
    *) printf '{}\n' ;;
  esac
  exit 0
}

BIN=$(command -v brain 2>/dev/null || true)
if [ -z "$BIN" ] && [ -x "$HOME/.local/bin/brain" ]; then
  BIN="$HOME/.local/bin/brain"
fi
if [ -z "$BIN" ]; then
  nothing
fi

out=$("$BIN" hook "$1" --format "$FORMAT" 2>/dev/null) || out=""

# `text` has no envelope to validate and is allowed to be empty -- that is how it
# says "nothing to add". The JSON envelopes are checked for actually being JSON,
# because a harness parsing stdout is the one caller that cannot recover from
# anything else appearing there.
if [ "$FORMAT" = "text" ]; then
  printf '%s' "$out"
  exit 0
fi

case "$out" in
  '{'*) printf '%s\n' "$out" ;;
  *) nothing ;;
esac
exit 0
