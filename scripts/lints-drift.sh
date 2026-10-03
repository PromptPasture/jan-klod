#!/bin/sh
# Drift check for the [workspace.lints] blocks.
#
# Cargo cannot inherit lints across workspaces, and this repository has three
# (the host, the guests, the GUI shell), each declaring the same policy. Nothing
# else keeps them equal, so a lint tightened in one and forgotten in the others
# makes "one policy for every package" a claim the tree does not back.
#
# Finds the manifests by content, not by path, so it survives package moves.
# Compares the declared lints only: comments and alignment may differ per file.
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
