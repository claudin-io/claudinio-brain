#!/bin/sh
# Runs `brain hook <what>` for a harness lifecycle event, and is careful about
# the ways that can go wrong in somebody else's session.
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
# 3. The arguments are not what this expected. This wrapper swallows stderr by
#    design, so an argument it mishandles does not error -- it goes silent. That
#    happened: `--global` used to be read as the output *format*, which turned
#    into `brain hook context --format --global`, which the CLI rejected, which
#    left a user's global brain silently uninjected in every session. So the
#    arguments are parsed rather than counted: the first positional is the event,
#    the second is the format, and every `--flag` is passed through to the binary
#    wherever it appears. `tests/step26_wrapper.rs` pins this contract.
#
# Empty output means the same as `{}` and is printed as such: a hook that exits 0
# with nothing at all is a hook the harness has to guess about.
set -u

# Positionals: <what> then <format>. Flags: forwarded verbatim, in order, after
# the positionals are peeled off. The three that carry a value (`--use`,
# `--brain`, `--prompt`) are known by name, because their value must not be
# mistaken for a positional; `--flag=value` needs no such care.
#
# POSIX sh has no arrays, so the flags are collected by appending them back onto
# the one list it does have: each argument is examined at the front and the ones
# to forward are pushed onto the back, counted, and kept once the originals run
# out. N is what remains to examine; EXTRA is what has been pushed.
WHAT=
FORMAT=
N=$#
EXTRA=0
while [ "$N" -gt 0 ]; do
  case "$1" in
    --use|--brain|--prompt)
      if [ "$N" -gt 1 ]; then
        set -- "$@" "$1" "$2"
        EXTRA=$((EXTRA + 2))
        shift
        N=$((N - 1))
      else
        set -- "$@" "$1"
        EXTRA=$((EXTRA + 1))
      fi
      ;;
    --*)
      set -- "$@" "$1"
      EXTRA=$((EXTRA + 1))
      ;;
    *)
      if [ -z "$WHAT" ]; then WHAT=$1; elif [ -z "$FORMAT" ]; then FORMAT=$1; fi
      ;;
  esac
  shift
  N=$((N - 1))
done
# Only the forwarded flags remain in "$@" now.
FORMAT=${FORMAT:-claude}

# What "nothing to say" looks like, which is not the same sentence in every
# envelope: a harness parsing JSON needs an empty object, and one splicing stdout
# into a prompt needs an empty *file*.
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

# No event named is a misconfiguration, but the rule for a hook is absolute:
# it never fails into somebody's session.
if [ -z "$WHAT" ]; then
  nothing
fi

BIN=$(command -v brain 2>/dev/null || true)
if [ -z "$BIN" ] && [ -x "$HOME/.local/bin/brain" ]; then
  BIN="$HOME/.local/bin/brain"
fi
if [ -z "$BIN" ]; then
  nothing
fi

# `capture` is the one hook that writes rather than answers, so it takes no
# envelope: what it has to say it says to the brain. It is run for its effect and
# then the harness is told the same nothing every other quiet hook tells it. The
# forwarded flags still apply -- capture writes to a brain, and which brain is
# exactly what a selector says.
if [ "$WHAT" = "capture" ]; then
  "$BIN" hook capture "$@" >/dev/null 2>&1 || true
  nothing
fi

out=$("$BIN" hook "$WHAT" --format "$FORMAT" "$@" 2>/dev/null) || out=""

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
