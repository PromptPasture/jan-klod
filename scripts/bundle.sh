#!/usr/bin/env sh
# Assemble a self-contained jan-klod bundle: core binary + selected guests +
# pre-filled config + README, tarred as jan-klod-<version>-<os>-<arch>.tar.gz.
#
# Usage: bundle.sh <core-binary> <ext-dir> <config> <out-dir>
set -eu

GATEWAY_BIN="$1"
UI_BIN="$2"
EXT_DIR="$3"
CONFIG="$4"
OUT="$5"

VERSION="${JK_VERSION:-0.1.0}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
NAME="jan-klod-${VERSION}-${OS}-${ARCH}"
DIR="${OUT}/${NAME}"

rm -rf "$DIR"
mkdir -p "$DIR/ext"
cp "$GATEWAY_BIN" "$DIR/jan-klod-gateway"
cp "$UI_BIN" "$DIR/jan-klod"
cp "$CONFIG" "$DIR/config.yaml"
# Exclude tool-escape-probe: a test-only sandbox-escape instrument, inert
# unless enabled, but a bad look to ship in a release.
if [ -d "$EXT_DIR" ]; then
	find "$EXT_DIR" -maxdepth 1 -name '*.wasm' \
		! -name 'tool-escape-probe.wasm' \
		-exec cp {} "$DIR/ext/" \;
fi

# Verify the assembled bundle before tarring: a missing guest degrades silently
# (the runtime just skips what it can't find), so check with the real resolver
# instead of shipping a bundle that's quietly less capable than its config claims.
#
# API key env vars are placeholders — verify never calls a provider, but config
# expansion fails on an unset key.
( cd "$DIR" && OPENAI_API_KEY="${OPENAI_API_KEY:-unset}" \
	ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-unset}" \
	GROQ_API_KEY="${GROQ_API_KEY:-unset}" \
	SEARCH_API_KEY="${SEARCH_API_KEY:-unset}" \
	./jan-klod-gateway verify config.yaml ext >/dev/null ) || {
	echo "bundle: FAILED — the assembled bundle is incomplete (see above)" >&2
	echo "bundle: run 'make ext' so every guest the config enables is staged" >&2
	exit 1
}

cat > "$DIR/README.md" <<EOF
# jan-klod ${VERSION} (${OS}-${ARCH})

Self-contained bundle: the core binary, its guest extensions (\`ext/\`), and a
pre-filled \`config.yaml\`. Persistence and the REST surface are built into the
core — no separate store/REST guests.

## Run

\`\`\`sh
./jan-klod my-session    # starts the gateway automatically if not running
\`\`\`

Or manually:

\`\`\`sh
./jan-klod-gateway serve config.yaml ext 127.0.0.1:8787
./jan-klod 127.0.0.1:8787 my-session
\`\`\`
EOF

tar -czf "${OUT}/${NAME}.tar.gz" -C "$OUT" "$NAME"
echo "bundle: ${OUT}/${NAME}.tar.gz"
