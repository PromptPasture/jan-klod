#!/bin/sh
# Stage one distribution's guests and their manifests from <ext-src> into <out-dir>.
#
# Usage: dist-stage.sh <dist> <ext-src> <out-dir>
#
# A listed guest that is not staged is an error, never a smaller archive.
set -eu

DIST="${1:?usage: dist-stage.sh <dist> <ext-src> <out-dir>}"
SRC="${2:?}"
OUT="${3:?}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIST="$ROOT/scripts/distributions/$DIST/guests"

[ -f "$LIST" ] || { echo "dist-stage: no distribution named '$DIST' ($LIST)" >&2; exit 2; }

rm -rf "$OUT"
mkdir -p "$OUT"

count=0
while IFS= read -r line; do
    name="${line%%#*}"
    name="$(printf '%s' "$name" | tr -d '[:space:]')"
    [ -n "$name" ] || continue
    [ -f "$SRC/$name.wasm" ] || {
        echo "dist-stage: $DIST lists '$name', which is not staged in $SRC — run \`make extensions\`" >&2
        exit 1
    }
    cp "$SRC/$name.wasm" "$OUT/"
    # Boot refuses a component without its manifest.
    [ -f "$SRC/$name.manifest.toml" ] || {
        echo "dist-stage: $name has no manifest in $SRC — run \`make extensions\`" >&2
        exit 1
    }
    cp "$SRC/$name.manifest.toml" "$OUT/"
    count=$((count + 1))
done < "$LIST"

echo "dist-stage: $DIST -> $count guest(s) in $OUT"
