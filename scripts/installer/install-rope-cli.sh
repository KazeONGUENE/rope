#!/bin/sh
# install-rope-cli.sh - Datachain Rope CLI installer.
#
# One-line install:
#   curl -fsSL https://get.datachain.network | sh
#
# Or pin an explicit version:
#   curl -fsSL https://get.datachain.network | sh -s -- --version 0.1.0
#
# Environment overrides (mostly for testing / air-gapped installs):
#   ROPE_INSTALL_BASE - defaults to https://get.datachain.network
#   ROPE_INSTALL_PREFIX - defaults to /usr/local (falls back to $HOME/.local
#                         when the user cannot write to /usr/local/bin)
#   ROPE_INSTALL_VERSION - defaults to "latest"; a symlink or file called
#                         `latest.txt` on the server maps to a concrete
#                         semver directory.
#
# Security model:
#   * Every published binary has an accompanying SHA256SUMS file signed by
#     the Foundation. This installer downloads BOTH, recomputes the SHA256,
#     and refuses to install if the value does not match.
#   * We NEVER curl | sh straight into a binary; the shell script is small,
#     readable, and only executes `install` after a checksum match.
#   * We NEVER assume root. If /usr/local/bin is not writable we install
#     into $HOME/.local/bin and print a PATH hint.
#
# The installer is intentionally POSIX-sh (not bash) so it runs unchanged
# on Debian/Ubuntu default /bin/sh (dash), Alpine (ash), and macOS.

set -eu

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

INSTALL_BASE="${ROPE_INSTALL_BASE:-https://get.datachain.network}"
INSTALL_VERSION="${ROPE_INSTALL_VERSION:-latest}"
INSTALL_PREFIX="${ROPE_INSTALL_PREFIX:-/usr/local}"

# Parse CLI flags. We support --version <x>, --prefix <p>, --help.
while [ "${1:-}" != "" ]; do
    case "$1" in
        --version)
            shift
            INSTALL_VERSION="${1:-latest}"
            ;;
        --version=*)
            INSTALL_VERSION="${1#--version=}"
            ;;
        --prefix)
            shift
            INSTALL_PREFIX="${1:-/usr/local}"
            ;;
        --prefix=*)
            INSTALL_PREFIX="${1#--prefix=}"
            ;;
        --help|-h)
            sed -n '2,30p' "$0" 2>/dev/null || true
            exit 0
            ;;
        *)
            echo "unknown argument: $1 (see --help)" >&2
            exit 2
            ;;
    esac
    shift
done

# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

say() { printf 'rope-cli: %s\n' "$*"; }
die() { printf 'rope-cli: fatal: %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

require() {
    for cmd in "$@"; do
        have "$cmd" || die "required tool '$cmd' is not installed"
    done
}

# HTTP GET with either curl or wget, whichever is present.
http_get() {
    src="$1"
    dst="$2"
    if have curl; then
        curl -fsSL --retry 3 --retry-delay 2 "$src" -o "$dst"
    elif have wget; then
        wget -q -O "$dst" "$src"
    else
        die "need curl or wget to download from $src"
    fi
}

# Portable SHA256 that works on Linux (sha256sum) and macOS (shasum -a 256).
sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif have openssl; then
        openssl dgst -sha256 "$1" | awk '{print $NF}'
    else
        die "need one of: sha256sum, shasum, openssl (for SHA-256)"
    fi
}

# ---------------------------------------------------------------------------
# Detect target triple
# ---------------------------------------------------------------------------

detect_target() {
    uname_s="$(uname -s 2>/dev/null || echo unknown)"
    uname_m="$(uname -m 2>/dev/null || echo unknown)"
    case "$uname_s" in
        Linux)
            os="unknown-linux-gnu"
            ;;
        Darwin)
            os="apple-darwin"
            ;;
        *)
            die "unsupported OS: $uname_s (Linux and macOS supported today)"
            ;;
    esac
    case "$uname_m" in
        x86_64|amd64)
            arch="x86_64"
            ;;
        aarch64|arm64)
            arch="aarch64"
            ;;
        *)
            die "unsupported CPU architecture: $uname_m"
            ;;
    esac
    echo "${arch}-${os}"
}

# ---------------------------------------------------------------------------
# Resolve version
# ---------------------------------------------------------------------------

resolve_version() {
    if [ "$INSTALL_VERSION" = "latest" ]; then
        tmp_ver="$1"
        http_get "$INSTALL_BASE/latest.txt" "$tmp_ver"
        v="$(tr -d '[:space:]' < "$tmp_ver")"
        rm -f "$tmp_ver"
        case "$v" in
            ""|"<"*|"404"*)
                die "server did not return a valid latest version (got: '$v')"
                ;;
        esac
        echo "$v"
    else
        echo "$INSTALL_VERSION"
    fi
}

# ---------------------------------------------------------------------------
# Pick install directory
# ---------------------------------------------------------------------------

pick_bindir() {
    prefixed_bin="$INSTALL_PREFIX/bin"
    if mkdir -p "$prefixed_bin" 2>/dev/null && \
       [ -w "$prefixed_bin" ]; then
        echo "$prefixed_bin"
        return 0
    fi
    # Fall back to a user-writable location.
    user_bin="$HOME/.local/bin"
    mkdir -p "$user_bin"
    echo "$user_bin"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

require uname awk mkdir mv chmod
target="$(detect_target)"
say "detected target: $target"

workdir="$(mktemp -d 2>/dev/null || mktemp -d -t rope-install)"
trap 'rm -rf "$workdir"' EXIT INT TERM

version="$(resolve_version "$workdir/latest.txt")"
say "installing rope-cli version: $version ($INSTALL_VERSION)"

url_base="$INSTALL_BASE/dist/$version"
tarball_name="rope-cli-$version-$target.tar.gz"
tarball_url="$url_base/$tarball_name"
sums_url="$url_base/SHA256SUMS"

say "downloading $tarball_url"
http_get "$tarball_url" "$workdir/$tarball_name"
say "downloading $sums_url"
http_get "$sums_url" "$workdir/SHA256SUMS"

actual="$(sha256_of "$workdir/$tarball_name")"
expected="$(awk -v n="$tarball_name" '$2 == n || $2 == "*"n { print $1 }' "$workdir/SHA256SUMS" | head -n1)"
if [ -z "$expected" ]; then
    die "SHA256SUMS does not list $tarball_name (server-side release incomplete)"
fi
if [ "$actual" != "$expected" ]; then
    die "checksum mismatch for $tarball_name: expected $expected got $actual"
fi
say "checksum ok"

# Extract into workdir. We expect a top-level `rope-cli-<version>-<target>/`
# directory containing the `rope` binary (and, in future, `rope-agent` etc.).
tar -xzf "$workdir/$tarball_name" -C "$workdir"
extracted_dir="$workdir/rope-cli-$version-$target"
if [ ! -x "$extracted_dir/rope" ]; then
    die "unexpected archive layout: missing '$extracted_dir/rope'"
fi

bindir="$(pick_bindir)"
say "installing to: $bindir/rope"
mv -f "$extracted_dir/rope" "$bindir/rope"
chmod +x "$bindir/rope"

# Optional companion binaries - install them side-by-side if present.
for extra in rope-agent rope-deployer; do
    if [ -x "$extracted_dir/$extra" ]; then
        mv -f "$extracted_dir/$extra" "$bindir/$extra"
        chmod +x "$bindir/$extra"
        say "installed side-by-side: $bindir/$extra"
    fi
done

say "done. Verify:  '$bindir/rope' --version"

case ":${PATH:-}:" in
    *":$bindir:"*)
        ;;
    *)
        printf '\nrope-cli: NOTE - %s is not in your PATH.\n' "$bindir"
        printf '           Add:  export PATH="%s:$PATH"\n' "$bindir"
        ;;
esac

# Print a small next-steps hint that references the console flow the
# reply-kj-stevens-2026-08-30 handover established as the canonical
# path (not `cargo install`, not `git clone`).
cat <<'HINT'

Next steps:
  * Sign in to https://console.datachain.network/console/ with your
    Datawallet+ account.
  * From the console, click "+ Deploy Node" to provision a Rope node
    on DigitalOcean or Exoscale in a couple of clicks - the console
    calls the same API this CLI wraps.
  * Or run `rope --help` locally to see the available subcommands.

Source: https://github.com/KazeONGUENE/rope
Docs:   https://datachain.network/docs
HINT
