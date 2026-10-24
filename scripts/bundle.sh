#!/usr/bin/env sh
# Assemble a self-contained bundle (binaries, guests, config, README) and tar it
# as jan-klod-<version>-<os>-<arch>.tar.gz.
#
# Usage: bundle.sh <core-binary> <ui-binary> <ext-dir> <config> <out-dir> [<skills-dir>]
#
# Optional environment:
#   JK_DIST     the distribution's name, which the archive name carries
set -eu

GATEWAY_BIN="$1"
UI_BIN="$2"
EXT_DIR="$3"
CONFIG="$4"
OUT="$5"
SKILLS_DIR="${6:-}"

VERSION="${JK_VERSION:-0.1.0}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
# Per-distribution archive names, so `make bundle DIST=` runs do not overwrite each other.
SUFFIX="${JK_DIST:+-$JK_DIST}"
NAME="jan-klod-${VERSION}-${OS}-${ARCH}${SUFFIX}"
DIR="${OUT}/${NAME}"

rm -rf "$DIR"
mkdir -p "$DIR/ext"
cp "$GATEWAY_BIN" "$DIR/jan-klod-gateway"
cp "$UI_BIN" "$DIR/jan-klod"
cp "$CONFIG" "$DIR/config.yaml"
# tool-escape-probe is a test-only sandbox-escape instrument; never ship it.
# Manifests travel with their components: boot refuses one without its manifest.
if [ -d "$EXT_DIR" ]; then
	find "$EXT_DIR" -maxdepth 1 \
		\( -name '*.wasm' -o -name '*.manifest.toml' \) \
		! -name 'tool-escape-probe.*' \
		-exec cp {} "$DIR/ext/" \;
fi

if [ -n "$SKILLS_DIR" ] && [ -d "$SKILLS_DIR" ]; then
	mkdir -p "$DIR/skills"
	find "$SKILLS_DIR" -maxdepth 1 -name '*.md' -exec cp {} "$DIR/skills/" \;
fi

# Verify with the real resolver before tarring: a missing guest otherwise
# degrades silently at runtime.
# Key env vars are placeholders: verify never calls a provider, but config
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
