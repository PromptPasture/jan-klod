#!/usr/bin/env sh
# Install jan-klod from the latest GitHub release.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
#
# Installs to ~/.local/bin/jan-klod by default.
# Set INSTALL_DIR to override (e.g. INSTALL_DIR=/usr/local/bin ... | sh).
#
# Pick a distribution with --dist:
#   curl -sSL .../install.sh | sh -s -- --dist headless-chat
#
#   coding         (default) models, the interceptor set, file and git tools
#   headless-chat  a chat channel; nothing that touches the machine
#   minimal        one provider and the interceptor set
#   self-extend    coding, plus a shell so the agent can compile — commands
#                  are ON and confined; read the security model first
set -eu

REPO="PromptPasture/jan-klod"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

# Must match scripts/distributions/ and release.yml; a mismatch is a 404 for the
# user. `docs_match_config::the_installer_offers_the_distributions_the_release_builds` enforces it.
DISTRIBUTIONS="coding headless-chat minimal self-extend"
DIST="coding"

while [ $# -gt 0 ]; do
  case "$1" in
    --dist) DIST="${2:?--dist needs a name}"; shift 2 ;;
    --dist=*) DIST="${1#--dist=}"; shift ;;
    -h|--help)
      echo "usage: install.sh [--dist <name>]"
      echo "  name: ${DISTRIBUTIONS}"
      exit 0 ;;
    *) echo "error: unknown option: $1" >&2; exit 2 ;;
  esac
done

case " ${DISTRIBUTIONS} " in
  *" ${DIST} "*) ;;
  *) echo "error: unknown distribution: ${DIST} (want one of: ${DISTRIBUTIONS})" >&2; exit 2 ;;
esac

# Detect OS and architecture.
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

# release.yml names 64-bit ARM per OS (aarch64 on Linux, arm64 on macOS). Do not normalize.
case "${OS}/${ARCH}" in
  linux/x86_64)          ARCH="x86_64"  ;;
  linux/aarch64|linux/arm64) ARCH="aarch64" ;;
  darwin/x86_64)
    echo "error: Intel Macs are not supported — no archive is published for darwin/x86_64" >&2
    exit 1
    ;;
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

API="https://api.github.com/repos/${REPO}/releases/latest"
TAG=$(curl -sSfL "${API}" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
if [ -z "${TAG}" ]; then
  echo "error: could not determine latest release tag" >&2
  exit 1
fi

# The archive name carries the distribution, as `make bundle DIST=` names it.
# No unsuffixed fallback: an installer asking for an archive that older tags lack
# would 404 every previous version.
BUNDLE="jan-klod-${TAG}-${OS}-${ARCH}-${DIST}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${BUNDLE}"
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS.txt"

echo "Installing jan-klod ${TAG} (${OS}-${ARCH}, ${DIST}) …"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

curl -sSfL "${URL}" -o "${TMP}/${BUNDLE}"
curl -sSfL "${CHECKSUMS_URL}" -o "${TMP}/SHA256SUMS.txt"

# This checksum proves transit integrity only. SHA256SUMS.txt comes from the same
# release as the bundle, so it does not prove provenance; that needs a minisign
# signature, which is not installed everywhere, so verify by hand:
#
#   minisign -Vm <bundle> -P <key from the landing page>  (see docs/quickstart.md)
#
# macOS ships `shasum`, not `sha256sum`.
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

# Install the whole bundle: binaries alone boot with no extensions or tools.
tar -xzf "${TMP}/${BUNDLE}" -C "${TMP}"
EXTRACTED="${TMP}/jan-klod-${TAG}-${OS}-${ARCH}-${DIST}"
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

if ! "${INSTALL_DIR}/jan-klod-gateway" verify > /dev/null 2>&1; then
  echo "" >&2
  echo "warning: the installed components did not all verify. Run:" >&2
  echo "  ${INSTALL_DIR}/jan-klod-gateway verify" >&2
fi

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
