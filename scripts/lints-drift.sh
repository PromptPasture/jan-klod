#!/bin/sh
# Drift check for the [workspace.lints] blocks. Cargo cannot inherit lints across
# workspaces, so each workspace declares the same policy and this keeps them equal.
# Manifests are found by content, not path. Only lint entries are compared.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

normalise() {
    awk '
        /^\[/ { inblock = ($0 ~ /^\[workspace\.lints/) }
        inblock {
            sub(/#.*/, "")
            gsub(/[ \t]+/, "")
            if ($0 != "") print
        }
    ' "$1"
}

files="$(git grep -l '^\[workspace\.lints' -- '*Cargo.toml')"
[ -n "$files" ] || { echo "lints-drift: no manifest declares [workspace.lints]" >&2; exit 1; }

first=""
status=0
for f in $files; do
    if [ -z "$first" ]; then
        first="$f"
        normalise "$f" > "${TMPDIR:-/tmp}/lints-drift.ref.$$"
        trap 'rm -f "${TMPDIR:-/tmp}/lints-drift.ref.$$"' EXIT
        continue
    fi
    if ! normalise "$f" | diff -u "${TMPDIR:-/tmp}/lints-drift.ref.$$" - > /dev/null; then
        echo "lints-drift: $f declares different lints than $first:" >&2
        normalise "$f" | diff -u "${TMPDIR:-/tmp}/lints-drift.ref.$$" - >&2 || true
        status=1
    fi
done

[ "$status" -eq 0 ] && echo "lints-drift: $(echo "$files" | wc -l | tr -d ' ') manifests declare identical lints"
exit "$status"
