#!/usr/bin/env sh
# Assemble a self-contained jan-klod bundle: the core binary + selected guests +
# a pre-filled config + a README, tarred as jan-klod-<version>-<os>-<arch>.tar.gz.
# Persistence, the REST surface, telegram, and delegation are host-side in the core
# binary (Phase 3/4), so ext/ holds only provider/interceptor/tool guests.
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
# Copy staged guests, minus the test instruments. `tool-escape-probe` exists to
# attempt a sandbox escape so the suite can watch it fail; it is inert unless
# someone enables it, but a release that ships a component named "escape probe"
# invites exactly one question and deserves not to.
if [ -d "$EXT_DIR" ]; then
	find "$EXT_DIR" -maxdepth 1 -name '*.wasm' \
		! -name 'tool-escape-probe.wasm' \
		-exec cp {} "$DIR/ext/" \;
fi

# Verify the assembled bundle before tarring it. A missing guest is otherwise a
# *silent* degradation: the runtime skips what it cannot find, so a bundle built
# without `make ext` (or with one guest that failed to compile) would ship, boot
# happily, and just be less capable than its own config claims. The core binary
# does the checking — it already owns config resolution, so the release gate uses
# the real resolver rather than a second, drifting copy of it in shell.
#
# The env vars are placeholders: `verify` only resolves and starts extensions, and
# no provider call is made, but config expansion must not fail on an unset key.
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
