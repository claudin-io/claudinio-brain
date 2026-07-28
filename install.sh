#!/bin/sh
# Install the `brain` binary. No Rust toolchain, no C compiler, no build.
#
#   curl -fsSL https://raw.githubusercontent.com/claudin-io/claudinio-brain/main/install.sh | sh
#
# Environment:
#   BRAIN_VERSION       release tag to install (default: newest stable, else `nightly`)
#   BRAIN_INSTALL_DIR   where to put the binary (default: ~/.local/bin)
#
# POSIX sh on purpose: this has to run under dash, ash and busybox, which is
# what a Debian or Alpine container gives you.
set -eu

REPO="claudin-io/claudinio-brain"
INSTALL_DIR="${BRAIN_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${BRAIN_VERSION:-}"

while [ $# -gt 0 ]; do
	case "$1" in
	--version)
		VERSION="${2:?--version needs a tag}"
		shift 2
		;;
	--dir)
		INSTALL_DIR="${2:?--dir needs a path}"
		shift 2
		;;
	-h | --help)
		cat <<-EOF
			install.sh -- install the \`brain\` binary (no build, no toolchain)

			  --version <tag>   release to install (default: newest stable, else \`nightly\`)
			  --dir <path>      where to put it (default: ~/.local/bin)

			The same two settings are readable as BRAIN_VERSION and BRAIN_INSTALL_DIR,
			which is how you pass them through \`curl ... | sh\`.
		EOF
		exit 0
		;;
	*)
		echo "install.sh: unknown argument: $1" >&2
		exit 2
		;;
	esac
done

say() { printf '%s\n' "$*"; }
die() {
	printf 'install.sh: %s\n' "$*" >&2
	exit 1
}

# --- download ----------------------------------------------------------------
# curl and wget are both common and neither is universal; picking once here
# keeps the rest of the script from caring which one exists.
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
	fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
	fetch_stdout() { wget -qO- "$1"; }
else
	die "needs curl or wget"
fi

# --- which binary ------------------------------------------------------------
os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
Darwin) ;;
Linux) ;;
MINGW* | MSYS* | CYGWIN* | Windows_NT)
	die "on Windows use install.ps1:
  irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex"
	;;
*) die "unsupported operating system: $os" ;;
esac

case "$arch" in
x86_64 | amd64) arch="x86_64" ;;
arm64 | aarch64) arch="aarch64" ;;
*) die "unsupported architecture: $arch" ;;
esac

# Linux is served by static musl builds, which have no glibc floor and so run on
# an eight-year-old distro and on Alpine alike.
case "$os" in
Darwin) target="${arch}-apple-darwin" ;;
Linux) target="${arch}-unknown-linux-musl" ;;
esac
asset="brain-${target}.tar.gz"

# --- which release -----------------------------------------------------------
if [ -z "$VERSION" ]; then
	# /releases/latest ignores prereleases, so this returns nothing at all until
	# the first `v*` tag is cut. That is the signal to fall back to nightly
	# rather than an error.
	VERSION="$(
		fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null |
			sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1
	)" || true
	[ -n "$VERSION" ] || VERSION="nightly"
fi

base="https://github.com/$REPO/releases/download/$VERSION"
say "brain: installing $VERSION ($target)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch "$base/$asset" "$tmp/$asset" ||
	die "no build of $VERSION for $target at $base/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" ||
	die "release $VERSION has no SHA256SUMS; refusing to install unverified"

# --- verify ------------------------------------------------------------------
if command -v sha256sum >/dev/null 2>&1; then
	sum="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
	sum="$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)"
else
	die "needs sha256sum or shasum to verify the download"
fi

want="$(sed -n "s/^\([0-9a-f]\{64\}\)  *\*\{0,1\}${asset}\$/\1/p" "$tmp/SHA256SUMS" | head -1)"
[ -n "$want" ] || die "$asset is not listed in SHA256SUMS"
[ "$sum" = "$want" ] || die "checksum mismatch for $asset
  expected $want
  got      $sum"

# --- install -----------------------------------------------------------------
tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
[ -f "$tmp/brain" ] || die "$asset did not contain a brain binary"

mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
# Land beside the target first, then rename. A rename within one directory is
# atomic and replaces the inode, so upgrading works even while another process
# is running the old binary -- writing over the file in place would fail with
# ETXTBSY instead.
staged="$INSTALL_DIR/.brain.$$"
cp "$tmp/brain" "$staged" ||
	die "could not write to $INSTALL_DIR (try: BRAIN_INSTALL_DIR=~/bin sh install.sh)"
chmod 755 "$staged"
mv -f "$staged" "$INSTALL_DIR/brain" || {
	rm -f "$staged"
	die "could not replace $INSTALL_DIR/brain"
}

say "brain: installed to $INSTALL_DIR/brain"
"$INSTALL_DIR/brain" --version

case ":${PATH}:" in
*":${INSTALL_DIR}:"*) ;;
*)
	say ""
	say "$INSTALL_DIR is not on your PATH. Add it:"
	say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc   # or ~/.bashrc"
	;;
esac
