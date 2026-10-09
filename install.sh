#!/usr/bin/env bash
# Install bi from a GitHub release.
#
#   curl -sSL https://raw.githubusercontent.com/qbart/bi/master/install.sh | bash
#
# Knobs (env vars, because a piped script cannot take arguments):
#   BI_VERSION   tag to install, e.g. v0.5.1   (default: latest release)
#   INSTALL_DIR  where the binary lands        (default: /usr/local/bin)
#
# See docs/specs/install.md.
set -euo pipefail

REPO="qbart/bi"
BIN="bi"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

die() {
	printf '%s: %s\n' "$BIN" "$1" >&2
	exit 1
}

note() { printf '%s\n' "$1"; }
warn() { printf 'warning: %s\n' "$1" >&2; }

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

# --- platform ---------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
Linux) ;;
Darwin) ;;
*) die "unsupported OS: $os. Build from source with: cargo build --release" ;;
esac

case "$os/$arch" in
Linux/x86_64 | Linux/amd64) target="x86_64-unknown-linux-gnu" ;;
Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
Darwin/arm64 | Darwin/aarch64) target="aarch64-apple-darwin" ;;
Darwin/x86_64)
	die "Intel Macs have no prebuilt binary yet (no x86_64-apple-darwin release asset).
Build from source with: cargo build --release"
	;;
*) die "unsupported platform: $os/$arch. Build from source with: cargo build --release" ;;
esac

# Linux binaries are built on Ubuntu 22.04, so glibc 2.35 is the floor. Warn
# before downloading rather than let the binary die at first run with a message
# that reads like a bug in bi.
if [ "$os" = Linux ] && command -v ldd >/dev/null 2>&1; then
	glibc="$(ldd --version 2>/dev/null | head -n1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
	if [ -n "$glibc" ]; then
		major="${glibc%%.*}"
		minor="${glibc##*.}"
		if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then
			warn "glibc $glibc is older than the 2.35 the Linux binaries need.
         bi may fail to start. Build from source with: cargo build --release"
		fi
	fi
fi

# --- version ----------------------------------------------------------------

version="${BI_VERSION:-}"
if [ -z "$version" ]; then
	# The /releases/latest redirect carries the tag and, unlike api.github.com,
	# is not rate-limited to 60 requests an hour per IP.
	resolved="$(curl -sSL -o /dev/null -w '%{url_effective}' \
		"https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
	version="${resolved##*/tag/}"
	case "$version" in
	v*) ;;
	*) version="" ;;
	esac
fi
if [ -z "$version" ]; then
	version="$(curl -sSfL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
		sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1 || true)"
fi
[ -n "$version" ] || die "could not resolve the latest release. Set BI_VERSION=vX.Y.Z"
case "$version" in
v*) ;;
*) version="v${version}" ;;
esac

# --- download ---------------------------------------------------------------

asset="${BIN}-${version}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/${version}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

note "Installing ${BIN} ${version} (${target}) to ${INSTALL_DIR}..."
curl -sSfL -o "$tmp/$asset" "${base}/${asset}" ||
	die "download failed: ${base}/${asset}
Check that ${version} exists and ships a ${target} build."
curl -sSfL -o "$tmp/sha256sums.txt" "${base}/sha256sums.txt" ||
	die "could not download sha256sums.txt for ${version}"

# --- verify -----------------------------------------------------------------

if command -v sha256sum >/dev/null 2>&1; then
	got="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
	got="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
else
	die "neither sha256sum nor shasum found; cannot verify the download"
fi

want="$(awk -v a="$asset" '$2 == a || $2 == "*" a {print $1}' "$tmp/sha256sums.txt" | head -n1)"
[ -n "$want" ] || die "$asset is not listed in sha256sums.txt"
[ "$got" = "$want" ] || die "checksum mismatch for $asset
  expected $want
  got      $got"

# --- install ----------------------------------------------------------------

tar -xzf "$tmp/$asset" -C "$tmp" --strip-components=1
[ -f "$tmp/$BIN" ] || die "the tarball did not contain a $BIN binary"
chmod +x "$tmp/$BIN"

sudo=""
if [ ! -d "$INSTALL_DIR" ]; then
	parent="$(dirname "$INSTALL_DIR")"
	[ -w "$parent" ] || sudo="sudo"
elif [ ! -w "$INSTALL_DIR" ]; then
	sudo="sudo"
fi
if [ -n "$sudo" ]; then
	command -v sudo >/dev/null 2>&1 ||
		die "$INSTALL_DIR is not writable and sudo is not available.
Re-run with INSTALL_DIR set to a directory you own, e.g. \$HOME/.local/bin"
	note "${INSTALL_DIR} needs root; you may be asked for your password."
fi

$sudo mkdir -p "$INSTALL_DIR"
$sudo install -m 755 "$tmp/$BIN" "$INSTALL_DIR/$BIN" 2>/dev/null ||
	{ $sudo cp "$tmp/$BIN" "$INSTALL_DIR/$BIN" && $sudo chmod 755 "$INSTALL_DIR/$BIN"; }

# curl does not set com.apple.quarantine, but a binary that arrived some other
# way might carry it. Cheap to clear, harmless when absent.
if [ "$os" = Darwin ] && command -v xattr >/dev/null 2>&1; then
	$sudo xattr -d com.apple.quarantine "$INSTALL_DIR/$BIN" 2>/dev/null || true
fi

# --- prove it works ---------------------------------------------------------

installed="$("$INSTALL_DIR/$BIN" --version 2>/dev/null || true)"
[ -n "$installed" ] || die "installed $INSTALL_DIR/$BIN but it would not run"

note "Installed ${installed} to ${INSTALL_DIR}/${BIN}"

case ":${PATH}:" in
*":${INSTALL_DIR}:"*) ;;
*)
	note ""
	note "${INSTALL_DIR} is not on your PATH. Add it:"
	note "  export PATH=\"${INSTALL_DIR}:\$PATH\""
	;;
esac

note ""
note "Next: ${BIN} config init   # writes ~/.config/${BIN}/config.toml"
