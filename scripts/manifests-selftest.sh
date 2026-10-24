#!/usr/bin/env sh
# Check that manifests-verify.sh can fail: tamper with a copy of the staged
# manifests four ways and require a refusal each time. A verifier that passes
# everything also passes on every build, so passing alone proves nothing.
#
# Usage: manifests-selftest.sh <ext-dir>
set -eu

EXT="$1"
ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
WORK="${TMPDIR:-/tmp}/jk-manifest-selftest-$$"
mkdir -p "$WORK"

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# No manifests (a clone without `make ext`) skips. JK_REQUIRE_GUESTS=1 turns the
# skip into a failure, so the gate cannot pass on a check that did nothing.
[ -f "$EXT/tool-fs.manifest.toml" ] || {
    if [ -n "${JK_REQUIRE_GUESTS:-}" ]; then
        echo "manifests-selftest: $EXT has no manifests and JK_REQUIRE_GUESTS is set" >&2
        exit 1
    fi
    echo "manifests-selftest: skipped — no manifests in $EXT (run 'make ext')"
    exit 0
}

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

# Two WIT versions have no api-version: the generator must refuse, not pick one.
mkdir -p "$WORK/wit"
cp "$ROOT/wit/host-fs.wit" "$ROOT/wit/host-log.wit" "$WORK/wit/"
# Replace whatever version wit/ declares, so the tamper stays a real disagreement
# after the version is bumped.
sed -i.bak 's/^package jan-klod:interfaces@.*;/package jan-klod:interfaces@9.9.9;/' \
    "$WORK/wit/host-log.wit"
rm -f "$WORK/wit/host-log.wit.bak"
if sh "$ROOT/scripts/manifests.sh" "$ROOT/pkgs/extensions" "$EXT" "$WORK/wit" tool-fs \
    >"$WORK/gen" 2>&1; then
    echo "manifests-selftest: disagreeing WIT versions were ACCEPTED" >&2
    exit 1
fi
# Non-zero exit alone is not enough: a broken wit-version.sh also fails. Require
# the refusal to be about the version.
grep -q 'more than one package version' "$WORK/gen" || {
    echo "manifests-selftest: the generator refused, but not over the version:" >&2
    cat "$WORK/gen" >&2
    exit 1
}
printf '  refused: %s\n' "disagreeing jan-klod:interfaces versions in wit/"

# Leave the staged manifests matching the real wit/.
sh "$ROOT/scripts/manifests.sh" "$ROOT/pkgs/extensions" "$EXT" "$ROOT/wit" tool-fs >/dev/null

echo "manifests-selftest: 5 refusals, as required"
