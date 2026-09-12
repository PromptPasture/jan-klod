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
#
# Add --gui for the archive that also carries the desktop window:
#   curl -sSL .../install.sh | sh -s -- --gui
#
# --gui is orthogonal to --dist, the way scripts/distributions/README.md says a
# client choice is: it selects the `-gui` archive of whichever distribution was
# asked for, not a distribution of its own.
set -eu

REPO="PromptPasture/jan-klod"
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"

# The distributions the release publishes. Kept in step with
# scripts/distributions/ and .github/workflows/release.yml by
# `docs_match_config::the_installer_offers_the_distributions_the_release_builds`
# — a name here that the release does not build is a 404 at the user, and the
# test is what turns that into a failing build instead.
DISTRIBUTIONS="coding headless-chat minimal"
DIST="coding"
# The `-gui` suffix `make bundle GUI=1` adds, or empty. Not a distribution: see
# the note at the top and scripts/distributions/README.md.
GUI_SUFFIX=""

while [ $# -gt 0 ]; do
  case "$1" in
    --dist) DIST="${2:?--dist needs a name}"; shift 2 ;;
    --dist=*) DIST="${1#--dist=}"; shift ;;
    --gui) GUI_SUFFIX="-gui"; shift ;;
    -h|--help)
      echo "usage: install.sh [--dist <name>] [--gui]"
      echo "  name: ${DISTRIBUTIONS}"
      echo "  --gui: also install the desktop window (jan-klod --gui)"
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

# The archive carries the distribution, as `make bundle DIST=` names it.
#
# There is no unsuffixed fallback, and that is safe rather than brave: nothing
# is tagged or released yet, so no published asset has the old name. Making the
# same change *after* a release would have needed one — an installer that asks
# for an archive no existing tag has turns every previous version into a 404,
# which is the one failure a user cannot work around.
BUNDLE="jan-klod-${TAG}-${OS}-${ARCH}-${DIST}${GUI_SUFFIX}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${BUNDLE}"
CHECKSUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS.txt"

echo "Installing jan-klod ${TAG} (${OS}-${ARCH}, ${DIST}${GUI_SUFFIX}) …"

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
EXTRACTED="${TMP}/jan-klod-${TAG}-${OS}-${ARCH}-${DIST}${GUI_SUFFIX}"
DATA_DIR="${DATA_DIR:-$(dirname "${INSTALL_DIR}")/share/jan-klod}"

mkdir -p "${INSTALL_DIR}" "${DATA_DIR}"
cp "${EXTRACTED}/jan-klod" "${INSTALL_DIR}/jan-klod"
cp "${EXTRACTED}/jan-klod-gateway" "${INSTALL_DIR}/jan-klod-gateway"
chmod +x "${INSTALL_DIR}/jan-klod" "${INSTALL_DIR}/jan-klod-gateway"
# The window, when this archive carries one. Beside the others because that is
# how `jan-klod --gui` finds it: sibling of the running executable, then PATH.
if [ -f "${EXTRACTED}/jan-klod-gui" ]; then
  cp "${EXTRACTED}/jan-klod-gui" "${INSTALL_DIR}/jan-klod-gui"
  chmod +x "${INSTALL_DIR}/jan-klod-gui"
fi
cp "${EXTRACTED}/config.yaml" "${DATA_DIR}/config.yaml"
rm -rf "${DATA_DIR}/ext"
cp -R "${EXTRACTED}/ext" "${DATA_DIR}/ext"

echo "Installed: ${INSTALL_DIR}/jan-klod"
echo "Installed: ${INSTALL_DIR}/jan-klod-gateway"
[ -f "${INSTALL_DIR}/jan-klod-gui" ] && echo "Installed: ${INSTALL_DIR}/jan-klod-gui"
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
if [ -f "${INSTALL_DIR}/jan-klod-gui" ]; then
  echo "  jan-klod --gui          # the same client, in a window"
fi
