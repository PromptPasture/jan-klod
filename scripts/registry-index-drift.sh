#!/usr/bin/env sh
# Check that `index.json` is a function of the staged components and nothing
# else — and that this check can actually fail.
#
# Same shape as scripts/web-dist-drift.sh and ext-new-selftest.sh: regenerate,
# compare, fail on a difference. What is different here is what it compares
# against. `src/web/dist/` is committed, so its drift check diffs a rebuild
# against the committed bytes. `ext/*.wasm` and `ext/*.manifest.toml` are
# **not** committed (see .gitignore), and a Rust wasm build is not byte-stable
# across machines, so a committed index.json would carry digests that differ per
# clone and drift on every run. The property worth holding is therefore not
# "matches what is committed" but **"two runs over one directory agree"**, which
# is what an index published from a release needs to be reproducible at all.
#
# Two runs agreeing is necessary and not sufficient: an entry order taken from
# the filesystem can agree twice by luck. So sortedness is asserted as a
# property too, rather than left to the comparison to notice.
#
# And both checks are probed, because a checker that cannot fail is decoration:
# a deliberately non-deterministic generator must be refused by the first, and a
# deliberately unsorted index by the second. That is the same argument
# manifests-selftest.sh makes about manifests-verify.sh.
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

command -v jq >/dev/null 2>&1 || {
    echo "registry-index-drift: jq is required" >&2
    exit 1
}

# Nothing staged is a legitimate state — a clone where nobody has run `make ext`
# yet — so this skips rather than fails, exactly as manifests-selftest.sh does,
# and with the same escape hatch: JK_REQUIRE_GUESTS=1 turns the skip into a
# failure so a gate cannot go green on a check that quietly did nothing.
if ! ls "$EXT"/*.manifest.toml >/dev/null 2>&1; then
    if [ -n "${JK_REQUIRE_GUESTS:-}" ]; then
        echo "registry-index-drift: $EXT has no manifests and JK_REQUIRE_GUESTS is set" >&2
        exit 1
    fi
    echo "registry-index-drift: skipped — no manifests in $EXT (run 'make ext')"
    exit 0
fi

# Run a generator twice and say whether the two outputs are identical. The
# generator is a command taking the output path as its last argument, so the
# probe below can pass a different one.
#
# The second run happens from another working directory. A generator that
# resolved anything relative to where it was invoked would produce a different
# index under `make` than under a release workflow, and that is exactly the kind
# of difference a published artifact must not have.
twice_agrees() {
    rm -f "$WORK/first.json" "$WORK/second.json"
    "$@" "$WORK/first.json" >/dev/null || return 1
    (cd "$WORK" && "$@" "$WORK/second.json" >/dev/null) || return 1
    diff -u "$WORK/first.json" "$WORK/second.json" >"$WORK/diff" 2>&1
}

# Entries sorted by name, and no name twice. Two runs can agree on a wrong
# order; nothing can agree on a duplicate.
sorted_and_unique() {
    jq -e '
        [.extensions[].name] as $names
        | ($names == ($names | sort)) and (($names | unique | length) == ($names | length))
    ' "$1" >/dev/null 2>&1
}

# A function rather than a command string in a variable: the arguments are
# paths, and a variable expanded unquoted would split one containing a space.
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
    jq -r '[.extensions[].name] | @json' "$WORK/first.json" >&2
    exit 1
fi
echo "registry-index-drift: entries sorted by name, names unique"

# --- The probes: each check above, shown failing on something that should fail.

# A generator that answers differently every run. A counter file rather than a
# timestamp: BSD `date` has no `%N`, so two runs inside one second would produce
# the same string and the probe would pass by accident — which is precisely the
# bug the probe exists to rule out.
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
