#!/bin/sh
# aas installer — downloads the prebuilt static binary from GitHub Releases.
# No Node, no runtime: a single executable.
#
#   curl -fsSL https://raw.githubusercontent.com/open330/aas/main/install.sh | sh
#
# Env overrides:
#   AAS_VERSION=v0.1.0   pin a version (default: latest)
#   AAS_BIN_DIR=~/.local/bin   install location

set -eu

REPO="open330/aas"
BIN="aas"

log() { printf '%s\n' "$*" >&2; }
die() { log "error: $*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64)        target="x86_64-apple-darwin" ;;
      *) die "unsupported macOS arch: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64|amd64)  target="x86_64-unknown-linux-musl" ;;
      arm64|aarch64) target="aarch64-unknown-linux-musl" ;;
      *) die "unsupported Linux arch: $arch" ;;
    esac ;;
  *) die "unsupported OS: $os (use install.ps1 on Windows)" ;;
esac

asset="${BIN}-${target}.tar.gz"
version="${AAS_VERSION:-latest}"
if [ "$version" = "latest" ]; then
  url="https://github.com/${REPO}/releases/latest/download/${asset}"
else
  url="https://github.com/${REPO}/releases/download/${version}/${asset}"
fi

# Pick an install dir on PATH (writable), preferring user-local.
bindir="${AAS_BIN_DIR:-}"
if [ -z "$bindir" ]; then
  for d in "$HOME/.local/bin" "$HOME/bin" "/usr/local/bin"; do
    if [ -d "$d" ] && [ -w "$d" ]; then bindir="$d"; break; fi
  done
  [ -z "$bindir" ] && bindir="$HOME/.local/bin"
fi
mkdir -p "$bindir"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

log "Downloading $asset ..."
if have curl; then
  curl -fsSL "$url" -o "$tmp/$asset" || die "download failed: $url"
elif have wget; then
  wget -qO "$tmp/$asset" "$url" || die "download failed: $url"
else
  die "need curl or wget"
fi

tar -xzf "$tmp/$asset" -C "$tmp" || die "extract failed"
# archive may contain the bare binary or a dir; find it.
binpath="$(find "$tmp" -type f -name "$BIN" -perm -u+x 2>/dev/null | head -n1)"
[ -z "$binpath" ] && binpath="$(find "$tmp" -type f -name "$BIN" | head -n1)"
[ -z "$binpath" ] && die "binary '$BIN' not found in archive"

install -m 0755 "$binpath" "$bindir/$BIN" 2>/dev/null || { cp "$binpath" "$bindir/$BIN"; chmod 0755 "$bindir/$BIN"; }

log "Installed $BIN -> $bindir/$BIN"
case ":$PATH:" in
  *":$bindir:"*) ;;
  *) log "note: $bindir is not on your PATH — add:  export PATH=\"$bindir:\$PATH\"" ;;
esac
"$bindir/$BIN" --version >&2 || true
