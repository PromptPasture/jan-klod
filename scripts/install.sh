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

# release.yml spells 64-bit ARM differently per OS: `aarch64` on Linux,
# `arm64` on macOS. Don't normalize both to one value — that 404s on Mac.
case "${OS}/${ARCH}" in
  linux/x86_64)          ARCH="x86_64"  ;;
  linux/aarch64|linux/arm64) ARCH="aarch64" ;;
  darwin/x86_64)         ARCH="x86_64"  ;;
  darwin/arm64|darwin/aarch64) ARCH="arm64" ;;
  linux/*|darwin/*)
    echo "error: unsupported architecture: ${ARCH}" >&2
    exit 1
    ;;
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

# macOS ships `shasum`, not `sha256sum` — fall back to it.
if command -v sha256sum > /dev/null 2>&1; then
  SHA_CHECK="sha256sum -c -"
elif command -v shasum > /dev/null 2>&1; then
  SHA_CHECK="shasum -a 256 -c -"
else
  echo "error: no sha256sum or shasum available to verify the download" >&2
  exit 1
fi
cd "${TMP}"
grep "${BUNDLE}" SHA256SUMS.txt | ${SHA_CHECK}
cd - > /dev/null

# Extract and install everything the bundle carries — not just the binaries,
# or the installed agent boots with no extensions and no tools.
tar -xzf "${TMP}/${BUNDLE}" -C "${TMP}"
EXTRACTED="${TMP}/jan-klod-${TAG}-${OS}-${ARCH}"
DATA_DIR="${DATA_DIR:-$(dirname "${INSTALL_DIR}")/share/jan-klod}"

mkdir -p "${INSTALL_DIR}" "${DATA_DIR}"
cp "${EXTRACTED}/jan-klod" "${INSTALL_DIR}/jan-klod"
cp "${EXTRACTED}/jan-klod-gateway" "${INSTALL_DIR}/jan-klod-gateway"
chmod +x "${INSTALL_DIR}/jan-klod" "${INSTALL_DIR}/jan-klod-gateway"
cp "${EXTRACTED}/config.yaml" "${DATA_DIR}/config.yaml"
rm -rf "${DATA_DIR}/ext"
cp -R "${EXTRACTED}/ext" "${DATA_DIR}/ext"

echo "Installed: ${INSTALL_DIR}/jan-klod"
echo "Installed: ${INSTALL_DIR}/jan-klod-gateway"
echo "Installed: ${DATA_DIR}/ (config.yaml + $(ls "${DATA_DIR}/ext" | wc -l | tr -d " ") components)"

# Confirm every enabled extension actually loads.
if ! "${INSTALL_DIR}/jan-klod-gateway" verify > /dev/null 2>&1; then
  echo "" >&2
  echo "warning: the installed components did not all verify. Run:" >&2
  echo "  ${INSTALL_DIR}/jan-klod-gateway verify" >&2
fi

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
echo "  export OPENAI_API_KEY=sk-..."
echo "  jan-klod my-session"
