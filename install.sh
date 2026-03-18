#!/bin/sh
# Install omegon from GitHub Releases.
#
# Usage:
#   curl -fsSL https://omegon.styrene.dev/install.sh | sh
#
# Or directly from GitHub:
#   curl -fsSL https://raw.githubusercontent.com/styrene-lab/omegon-core/main/install.sh | sh
#
# Manual download:
#   https://github.com/styrene-lab/omegon-core/releases

set -e

REPO="styrene-lab/omegon-core"
BINARY="omegon"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# Detect platform
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  darwin) OS_NAME="darwin" ;;
  linux)  OS_NAME="linux" ;;
  *)
    echo "Error: unsupported OS: $OS"
    echo "Omegon supports macOS and Linux. Windows users: use WSL."
    exit 1
    ;;
esac

case "$ARCH" in
  arm64|aarch64) ARCH_NAME="arm64" ;;
  x86_64|amd64)  ARCH_NAME="x64" ;;
  *)
    echo "Error: unsupported architecture: $ARCH"
    exit 1
    ;;
esac

PLATFORM="${OS_NAME}-${ARCH_NAME}"
ARCHIVE="${BINARY}-${PLATFORM}.tar.gz"

# Get latest release tag
echo "Detecting latest release..."
LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')

if [ -z "$LATEST" ]; then
  echo "Error: could not determine latest release."
  echo "Check: https://github.com/${REPO}/releases"
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${LATEST}/${ARCHIVE}"

echo "Downloading ${BINARY} ${LATEST} for ${PLATFORM}..."
echo "  ${URL}"

# Download and extract
TMP=$(mktemp -d)
curl -fsSL "$URL" -o "${TMP}/${ARCHIVE}"
tar xzf "${TMP}/${ARCHIVE}" -C "$TMP"

# Install
if [ -w "$INSTALL_DIR" ]; then
  mv "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
fi

chmod +x "${INSTALL_DIR}/${BINARY}"
rm -rf "$TMP"

echo ""
echo "✓ Installed ${BINARY} ${LATEST} to ${INSTALL_DIR}/${BINARY}"
echo ""
echo "Get started:"
echo "  # With API key:"
echo "  export ANTHROPIC_API_KEY=\"sk-ant-...\""
echo "  ${BINARY} --prompt \"hello world\""
echo ""
echo "  # With subscription (Claude Pro/Max):"
echo "  ${BINARY} login"
echo ""
echo "  # Interactive mode:"
echo "  ${BINARY} interactive"
