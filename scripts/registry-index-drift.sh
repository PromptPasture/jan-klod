#!/usr/bin/env sh
# Check that index.json is a function of the staged components, and that the
# check can fail. Components are not committed and wasm builds are not
# byte-stable, so the property is "two runs over one directory agree", not
# "matches the committed index". Sortedness is asserted separately: two runs can
# agree on an order that came from the filesystem by luck.
#
# Usage: registry-index-drift.sh <ext-dir> <base-url> <author>
set -eu

[ $# -eq 3 ] || {
    echo "usage: registry-index-drift.sh <ext-dir> <base-url> <author>" >&2
    exit 2
}

EXT="$1"
BASE_URL="$2"
AUTHOR="$3"

HERE="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(CDPATH='' cd -- "$HERE/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

command -v jaq >/dev/null 2>&1 || {
    echo 'registry-index-drift: jaq is required — run: make setup' >&2
    exit 1
}

# No manifests (a clone without `make ext`) skips. JK_REQUIRE_GUESTS=1 makes it fail.
if ! ls "$EXT"/*.manifest.toml >/dev/null 2>&1; then
    if [ -n "${JK_REQUIRE_GUESTS:-}" ]; then
        echo "registry-index-drift: $EXT has no manifests and JK_REQUIRE_GUESTS is set" >&2
        exit 1
    fi
    echo "registry-index-drift: skipped — no manifests in $EXT (run 'make ext')"
    exit 0
fi

# Run a generator twice (output path last) and compare. The second run starts in
# another directory, so a cwd-dependent generator fails here.
twice_agrees() {
    rm -f "$WORK/first.json" "$WORK/second.json"
    "$@" "$WORK/first.json" >/dev/null || return 1
    (cd "$WORK" && "$@" "$WORK/second.json" >/dev/null) || return 1
    diff -u "$WORK/first.json" "$WORK/second.json" >"$WORK/diff" 2>&1
}

sorted_and_unique() {
    jaq -e '
        [.extensions[].name] as $names
        | ($names == ($names | sort)) and (($names | unique | length) == ($names | length))
    ' "$1" >/dev/null 2>&1
}

# A function, not a command string: paths may contain spaces.
real_generator() {
    sh "$ROOT/scripts/registry-index.sh" "$EXT" "$BASE_URL" "$AUTHOR" "$@"
}

echo "registry-index-drift: generating twice from $EXT"
if ! twice_agrees real_generator; then
    echo "registry-index-drift: two runs over the same directory disagree" >&2
    echo "  a published index has to be reproducible; something here reads the clock," >&2
    echo "  the environment, or the filesystem's own ordering:" >&2
    cat "$WORK/diff" >&2
    exit 1
fi
echo "registry-index-drift: two runs agree, 0 drift"

if ! sorted_and_unique "$WORK/first.json"; then
    echo "registry-index-drift: the index is not sorted by name, or names repeat" >&2
    jaq -r '[.extensions[].name] | @json' "$WORK/first.json" >&2
    exit 1
fi
echo "registry-index-drift: entries sorted by name, names unique"

# Probes: each check must refuse a deliberately bad input.
# A counter file, not a timestamp: same-second runs would otherwise match.
cat >"$WORK/flaky.sh" <<'EOF'
set -eu
here="$(dirname -- "$0")"
count=$(cat "$here/count" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" >"$here/count"
printf '{ "index-version": 1, "generated-run": %s, "extensions": [] }\n' "$count" >"$1"
EOF

if twice_agrees sh "$WORK/flaky.sh"; then
    echo "registry-index-drift: a generator that changes every run was ACCEPTED" >&2
    echo "  the comparison is not comparing anything" >&2
    exit 1
fi
printf '  refused: %s\n' "a generator whose output changes between runs"

printf '%s\n' '{ "index-version": 1, "extensions": [ { "name": "tool-z" }, { "name": "tool-a" } ] }' \
    >"$WORK/unsorted.json"
if sorted_and_unique "$WORK/unsorted.json"; then
    echo "registry-index-drift: an index in the wrong order was ACCEPTED" >&2
    exit 1
fi
printf '  refused: %s\n' "entries out of name order"

printf '%s\n' '{ "index-version": 1, "extensions": [ { "name": "tool-a" }, { "name": "tool-a" } ] }' \
    >"$WORK/duplicate.json"
if sorted_and_unique "$WORK/duplicate.json"; then
    echo "registry-index-drift: an index naming one component twice was ACCEPTED" >&2
    exit 1
fi
printf '  refused: %s\n' "one component named twice"

echo "registry-index-drift: 3 refusals, as required"
