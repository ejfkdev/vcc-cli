#!/usr/bin/env sh
# VCC install script — downloads the latest binary for your platform
# Usage: curl -fsSL https://raw.githubusercontent.com/ejfkdev/vcc-cli/main/install.sh | sh

set -e

REPO="ejfkdev/vcc-cli"
INSTALL_DIR="${VCC_INSTALL_DIR:-/usr/local/bin}"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
    Darwin-arm64)  ARTIFACT="vcc-aarch64-macos" ;;
    Darwin-x86_64) ARTIFACT="vcc-x86_64-macos" ;;
    Linux-arm64)   ARTIFACT="vcc-aarch64-linux-musl" ;;
    Linux-aarch64) ARTIFACT="vcc-aarch64-linux-musl" ;;
    Linux-x86_64)  ARTIFACT="vcc-x86_64-linux-musl" ;;
    *) echo "Unsupported platform: $OS-$ARCH" >&2; exit 1 ;;
esac

# Get latest release tag
TAG="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/')"
if [ -z "$TAG" ]; then
    echo "Failed to determine latest version" >&2
    exit 1
fi

echo "Installing vcc v${TAG} for ${OS}-${ARCH}..."

# Download
URL="https://github.com/${REPO}/releases/download/v${TAG}/${ARTIFACT}"
TMPFILE="$(mktemp)"

if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMPFILE"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMPFILE" "$URL"
else
    echo "curl or wget required" >&2
    exit 1
fi

# Install
chmod +x "$TMPFILE"

if [ -w "$INSTALL_DIR" ]; then
    mv "$TMPFILE" "${INSTALL_DIR}/vcc"
else
    echo "Installing to ${INSTALL_DIR} requires sudo..."
    sudo mv "$TMPFILE" "${INSTALL_DIR}/vcc"
fi

echo "vcc v${TAG} installed to ${INSTALL_DIR}/vcc"
vcc --version
