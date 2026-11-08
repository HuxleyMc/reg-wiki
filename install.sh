#!/bin/sh
# Install or update reg-wiki from GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/HuxleyMc/reg-wiki/main/install.sh | sh
#
# Run it again to update. Options (also settable via environment):
#   --version <tag>    install a specific release, e.g. v0.2.0   (REG_WIKI_VERSION)
#   --dir <path>       install directory, default ~/.local/bin    (REG_WIKI_INSTALL_DIR)
#   --force            reinstall even if already up to date
#   --uninstall        remove the binary (run `reg-wiki uninstall` first to unwire agents)
set -eu

REPO="HuxleyMc/reg-wiki"
BIN="reg-wiki"
VERSION="${REG_WIKI_VERSION:-latest}"
INSTALL_DIR="${REG_WIKI_INSTALL_DIR:-$HOME/.local/bin}"
FORCE=0
UNINSTALL=0

say() { printf 'reg-wiki: %s\n' "$*"; }
die() { printf 'reg-wiki: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION="$2"; shift 2 ;;
    --version=*) VERSION="${1#*=}"; shift ;;
    --dir) [ $# -ge 2 ] || die "--dir needs a value"; INSTALL_DIR="$2"; shift 2 ;;
    --dir=*) INSTALL_DIR="${1#*=}"; shift ;;
    --force) FORCE=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help)
      echo "usage: install.sh [--version <tag>] [--dir <path>] [--force] [--uninstall]"
      exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

DEST="$INSTALL_DIR/$BIN"

if [ "$UNINSTALL" = 1 ]; then
  if [ -e "$DEST" ]; then
    rm -f "$DEST"
    say "removed $DEST"
  else
    say "nothing to remove at $DEST"
  fi
  exit 0
fi

# --- pick the release asset for this machine --------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) die "unsupported CPU architecture: $arch" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin)
    # An x86_64 shell under Rosetta on Apple silicon: prefer the native build.
    if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
      arch=aarch64
    fi
    target="$arch-apple-darwin" ;;
  *) die "unsupported OS: $os (Linux and macOS are supported)" ;;
esac

if has curl; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
  resolve_latest() { curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest"; }
elif has wget; then
  fetch() { wget -q -O "$2" "$1"; }
  resolve_latest() { wget -q -S --spider --max-redirect=5 "https://github.com/$REPO/releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tail -n1 | tr -d '\r'; }
else
  die "need curl or wget"
fi

# --- resolve the version ------------------------------------------------------
if [ "$VERSION" = latest ]; then
  url=$(resolve_latest) || die "no release found at github.com/$REPO (is the repo public, and has a release been published?)"
  VERSION="${url##*/}"
  case "$VERSION" in v*) ;; *) die "no published release found for $REPO" ;; esac
else
  case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac
fi

if [ "$FORCE" = 0 ] && [ -x "$DEST" ]; then
  current=$("$DEST" --version 2>/dev/null | awk '{print $2}' || true)
  if [ "v$current" = "$VERSION" ]; then
    say "already up to date ($VERSION) at $DEST"
    exit 0
  fi
  [ -n "$current" ] && say "updating v$current -> $VERSION"
fi

fresh=1
[ -e "$DEST" ] && fresh=0

# --- download, verify, install ------------------------------------------------
asset="$BIN-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$VERSION"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $asset ($VERSION)"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || die "checksum download failed"

expected=$(awk '{print $1}' "$tmp/$asset.sha256")
if has sha256sum; then
  actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
elif has shasum; then
  actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
else
  die "need sha256sum or shasum to verify the download"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
# Install via rename so a running reg-wiki (e.g. a hook) is never half-written.
cp "$tmp/$BIN" "$INSTALL_DIR/.$BIN.new"
chmod 755 "$INSTALL_DIR/.$BIN.new"
mv -f "$INSTALL_DIR/.$BIN.new" "$DEST"

say "installed $("$DEST" --version) to $DEST"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: $INSTALL_DIR is not on your PATH; add this to your shell profile:"
     printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR" ;;
esac

if [ "$fresh" = 1 ]; then
  say "next: run 'reg-wiki install' to wire it into your agents"
fi
