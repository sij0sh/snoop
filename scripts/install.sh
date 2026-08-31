#!/bin/sh
# snoop installer (POSIX sh).
# Usage: curl -fsSL https://raw.githubusercontent.com/colbymchenry/snoop/main/scripts/install.sh | sh
# Args (piped): ... | sh -s -- --dir <path> --version <tag|latest>
set -eu
(set -o pipefail) 2>/dev/null && set -o pipefail

REPO="${GITHUB_REPO:-colbymchenry/snoop}"
TAG="latest"
INSTALL_DIR="${SNOOP_INSTALL_DIR:-$HOME/.local/bin}"

usage() {
    cat <<EOF
snoop installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/$REPO/main/scripts/install.sh | sh
  install.sh --help | --dir <path> | --version <tag|latest>

Options:
  --dir <path>            Install directory (default: \$HOME/.local/bin,
                          overridable via SNOOP_INSTALL_DIR).
  --version <tag|latest>  Release tag to install (default: latest).
  --help                  Show this help.

Environment:
  GITHUB_REPO             Repo to download from (default: colbymchenry/snoop).
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        --dir)
            [ $# -ge 2 ] || { echo "error: --dir requires a path" >&2; exit 1; }
            INSTALL_DIR="$2"
            shift 2
            ;;
        --version)
            [ $# -ge 2 ] || { echo "error: --version requires a tag or 'latest'" >&2; exit 1; }
            TAG="$2"
            shift 2
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
done

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
    Darwin|Linux) ;;
    *)
        echo "error: unsupported OS '$os'. On Windows use install.ps1:" >&2
        echo "  https://github.com/$REPO#installation" >&2
        exit 1
        ;;
esac

case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
        echo "error: unsupported architecture '$arch'" >&2
        exit 1
        ;;
esac

case "$os:$arch" in
    Darwin:x86_64)  TARGET="x86_64-apple-darwin" ;;
    Darwin:aarch64) TARGET="aarch64-apple-darwin" ;;
    Linux:x86_64)   TARGET="x86_64-unknown-linux-gnu" ;;
    Linux:aarch64)  TARGET="aarch64-unknown-linux-gnu" ;;
esac

if [ "$TAG" = "latest" ]; then
    url="https://github.com/$REPO/releases/latest/download/snoop-$TARGET.tar.gz"
else
    url="https://github.com/$REPO/releases/download/$TAG/snoop-$TARGET.tar.gz"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM

archive="$tmp/snoop-$TARGET.tar.gz"
echo "Downloading snoop ($TARGET) from $url"

if command -v curl >/dev/null 2>&1; then
    curl -fsSL -o "$archive" "$url" || { echo "error: download failed: $url" >&2; exit 1; }
elif command -v wget >/dev/null 2>&1; then
    wget -O "$archive" "$url" || { echo "error: download failed: $url" >&2; exit 1; }
else
    echo "error: curl or wget is required to download" >&2
    exit 1
fi

[ -s "$archive" ] || { echo "error: asset not found or empty: $url" >&2; exit 1; }

tar -xzf "$archive" -C "$tmp"

bin=""
if [ -f "$tmp/snoop" ]; then
    bin="$tmp/snoop"
else
    for f in "$tmp"/*/snoop; do
        if [ -f "$f" ]; then
            bin="$f"
            break
        fi
    done
fi
[ -n "$bin" ] || { echo "error: snoop binary not found in archive" >&2; exit 1; }

mkdir -p "$INSTALL_DIR"
mv "$bin" "$INSTALL_DIR/snoop"
chmod +x "$INSTALL_DIR/snoop"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo ""
        echo "NOTE: $INSTALL_DIR is not on your PATH."
        echo "Add this line to ~/.bashrc, ~/.zshrc, or your shell's profile:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

if "$INSTALL_DIR/snoop" --version >/dev/null 2>&1; then
    echo "Installed: $("$INSTALL_DIR/snoop" --version 2>/dev/null | head -n 1)"
else
    echo "Installed to $INSTALL_DIR/snoop (could not run --version locally)"
fi

echo ""
echo "Next steps:"
echo "  1. Open a new terminal (or: export PATH=\"$INSTALL_DIR:\$PATH\")."
echo "  2. Run: snoop install   # wires up your coding agents"
echo "  3. In each project, run: snoop init"
