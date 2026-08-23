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

BIN=$(command -v brain 2>/dev/null || true)
if [ -z "$BIN" ] && [ -x "$HOME/.local/bin/brain" ]; then
  BIN="$HOME/.local/bin/brain"
fi
if [ -z "$BIN" ]; then
  printf '{}\n'
  exit 0
fi

out=$("$BIN" hook "$1" 2>/dev/null) || out=""
case "$out" in
  '{'*) printf '%s\n' "$out" ;;
  *) printf '{}\n' ;;
esac
exit 0
