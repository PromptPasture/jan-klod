#!/usr/bin/env sh
# Assemble a self-contained jan-klod bundle: the core binary + selected guests +
# a pre-filled config + a README, tarred as jan-klod-<version>-<os>-<arch>.tar.gz.
# Persistence, the REST surface, telegram, and delegation are host-side in the core
# binary (Phase 3/4), so ext/ holds only provider/interceptor/tool guests.
#
# Usage: bundle.sh <core-binary> <ext-dir> <config> <out-dir>
set -eu

CORE_BIN="$1"
EXT_DIR="$2"
CONFIG="$3"
OUT="$4"

VERSION="${JK_VERSION:-0.1.0}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
NAME="jan-klod-${VERSION}-${OS}-${ARCH}"
DIR="${OUT}/${NAME}"

rm -rf "$DIR"
mkdir -p "$DIR/ext"
cp "$CORE_BIN" "$DIR/jan-klod"
cp "$CONFIG" "$DIR/config.yaml"
# Copy staged guests, if any (a bundle with none still boots headless).
if [ -d "$EXT_DIR" ]; then
	find "$EXT_DIR" -maxdepth 1 -name '*.wasm' -exec cp {} "$DIR/ext/" \;
fi

cat > "$DIR/README.md" <<EOF
# jan-klod ${VERSION} (${OS}-${ARCH})

Self-contained bundle: the core binary, its guest extensions (\`ext/\`), and a
pre-filled \`config.yaml\`. Persistence and the REST surface are built into the
core — no separate store/REST guests.

## Run

\`\`\`sh
./jan-klod config.yaml ext            # boot + print the extension plan
./jan-klod serve config.yaml ext 127.0.0.1:8787   # serve turns over REST
\`\`\`
EOF

tar -czf "${OUT}/${NAME}.tar.gz" -C "$OUT" "$NAME"
echo "bundle: ${OUT}/${NAME}.tar.gz"
