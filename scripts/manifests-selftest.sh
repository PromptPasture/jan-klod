#!/usr/bin/env sh
# Check that the manifest checks can fail.
#
# `manifests-verify.sh` runs on every build and passes, which says nothing on
# its own: a verifier that accepts everything passes too. This tampers with a
# copy of the staged manifests, four ways, and requires a refusal each time —
# so the guard is a standing guarantee rather than something confirmed by hand
# once and trusted afterwards.
#
# Usage: manifests-selftest.sh <ext-dir>
set -eu

EXT="$1"
ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
WORK="${TMPDIR:-/tmp}/jk-manifest-selftest-$$"
mkdir -p "$WORK"

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# Absent manifests are a legitimate state — a clone where nobody has run
# `make ext` yet — so this skips rather than fails, the same way every
# integration test skips on unstaged guests. And the same escape hatch applies:
# `JK_REQUIRE_GUESTS=1` turns the skip into a failure, so the gate cannot go
# green on a check that quietly did nothing.
[ -f "$EXT/tool-fs.manifest.toml" ] || {
    if [ -n "${JK_REQUIRE_GUESTS:-}" ]; then
        echo "manifests-selftest: $EXT has no manifests and JK_REQUIRE_GUESTS is set" >&2
        exit 1
    fi
    echo "manifests-selftest: skipped — no manifests in $EXT (run 'make ext')"
    exit 0
}

# Each case: a description, and a sed applied to tool-fs's manifest.
refuses() {
    what="$1"
    shift
    cp "$EXT/tool-fs.manifest.toml" "$WORK/tool-fs.manifest.toml"
    "$@"
    if sh "$ROOT/scripts/manifests-verify.sh" "$WORK" tool-fs >"$WORK/out" 2>&1; then
        echo "manifests-selftest: $what was ACCEPTED — the guard does not guard" >&2
        cat "$WORK/out" >&2
        exit 1
    fi
    printf '  refused: %s\n' "$what"
}

add_capability() {
    sed -i.bak 's/^    "host-fs",$/    "host-fs",\n    "host-process",/' \
        "$WORK/tool-fs.manifest.toml"
}
add_export() {
    sed -i.bak 's/^    "host-fs",$/    "host-fs",\n    "tool-callable",/' \
        "$WORK/tool-fs.manifest.toml"
}
drop_key() {
    grep -v '^api-version' "$EXT/tool-fs.manifest.toml" >"$WORK/tool-fs.manifest.toml"
}
remove_manifest() {
    rm -f "$WORK/tool-fs.manifest.toml"
}

echo "manifests-selftest: each tampering must be refused"
refuses "a capability the component does not import" add_capability
refuses "an export listed as a capability" add_export
refuses "a manifest missing api-version" drop_key
refuses "a missing manifest" remove_manifest

# And the generator's own refusal: two WIT package versions have no answer to
# "which is the api-version", so it must not pick one.
mkdir -p "$WORK/wit"
cp "$ROOT/wit/host-fs.wit" "$ROOT/wit/host-log.wit" "$WORK/wit/"
sed -i.bak 's/@0\.1\.0;/@0.2.0;/' "$WORK/wit/host-log.wit"
rm -f "$WORK/wit/host-log.wit.bak"
if sh "$ROOT/scripts/manifests.sh" "$ROOT/src/extensions" "$EXT" "$WORK/wit" tool-fs \
    >"$WORK/gen" 2>&1; then
    echo "manifests-selftest: disagreeing WIT versions were ACCEPTED" >&2
    exit 1
fi
# A non-zero exit is not enough on its own. The version is read by a separate
# script now, so a missing `wit-version.sh` — or a typo in the path to it —
# fails non-zero too, and this case would report "refused" while the generator
# was merely broken. Require the refusal to be *about* the version.
grep -q 'more than one package version' "$WORK/gen" || {
    echo "manifests-selftest: the generator refused, but not over the version:" >&2
    cat "$WORK/gen" >&2
    exit 1
}
printf '  refused: %s\n' "disagreeing jan-klod:interfaces versions in wit/"

# The tampering above never touched $EXT, but a generator run just did — leave
# the staged manifests as the real `wit/` describes them.
sh "$ROOT/scripts/manifests.sh" "$ROOT/src/extensions" "$EXT" "$ROOT/wit" tool-fs >/dev/null

echo "manifests-selftest: 5 refusals, as required"
