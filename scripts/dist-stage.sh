#!/bin/sh
# Stage one distribution's guests into a directory a bundle can be built from.
#
# Usage: dist-stage.sh <dist> <ext-src> <out-dir>
#
# Copies each component named in scripts/distributions/<dist>/guests, and the
# manifest beside it, from <ext-src> into <out-dir>. A name that is not there is
# an error rather than a smaller archive: a distribution that silently ships
# fewer guests than it lists is a distribution nobody can reason about, and the
# thing it is missing is exactly what its user will reach for.
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
    # The manifest travels with the component. Without it boot refuses the
    # component outright (16a), so an archive missing one is an archive that
    # does not start.
    [ -f "$SRC/$name.manifest.toml" ] || {
        echo "dist-stage: $name has no manifest in $SRC — run \`make extensions\`" >&2
        exit 1
    }
    cp "$SRC/$name.manifest.toml" "$OUT/"
    count=$((count + 1))
done < "$LIST"

echo "dist-stage: $DIST -> $count guest(s) in $OUT"
