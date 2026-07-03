#!/usr/bin/env sh
# Install jan-klod from the latest GitHub release.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
#
# Installs to ~/.local/bin/jan-klod by default.
# Set INSTALL_DIR to override (e.g. INSTALL_DIR=/usr/local/bin ... | sh).
set -eu

REPO="PromptPasture/jan-klod"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

# Detect OS and architecture.
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${ARCH}" in
  x86_64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *)
    echo "error: unsupported architecture: ${ARCH}" >&2
    exit 1
    ;;
esac

case "${OS}" in
  linux)  ;;
  darwin) ;;
  *)
    echo "error: unsupported OS: ${OS}" >&2
    exit 1
    ;;
esac

# Find the latest release tag.
API="https://api.github.com/repos/${REPO}/releases/latest"
TAG=$(curl -sSfL "${API}" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
if [ -z "${TAG}" ]; then
  echo "error: could not determine latest release tag" >&2
  exit 1
fi

BUNDLE="jan-klod-${TAG}-${OS}-${ARCH}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${BUNDLE}"
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS.txt"

echo "Installing jan-klod ${TAG} (${OS}-${ARCH}) …"

# Download bundle + checksums to a temp dir.
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

curl -sSfL "${URL}" -o "${TMP}/${BUNDLE}"
curl -sSfL "${CHECKSUMS_URL}" -o "${TMP}/SHA256SUMS.txt"

# Verify checksum.
cd "${TMP}"
grep "${BUNDLE}" SHA256SUMS.txt | sha256sum -c -
cd - > /dev/null

# Extract and install.
tar -xzf "${TMP}/${BUNDLE}" -C "${TMP}"
EXTRACTED="${TMP}/jan-klod-${TAG}-${OS}-${ARCH}"

mkdir -p "${INSTALL_DIR}"
cp "${EXTRACTED}/jan-klod" "${INSTALL_DIR}/jan-klod"
chmod +x "${INSTALL_DIR}/jan-klod"

echo "Installed: ${INSTALL_DIR}/jan-klod"

# Check PATH.
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "Note: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your shell profile:"
    echo "  export PATH=\"\${HOME}/.local/bin:\${PATH}\""
    ;;
esac

echo ""
echo "Quick start:"
echo "  export ANTHROPIC_API_KEY=sk-..."
echo "  jan-klod serve config.yaml ext"
