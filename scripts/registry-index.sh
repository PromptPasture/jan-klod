#!/usr/bin/env sh
# Write index.json for a directory of staged components.
#
# The index is what a registry can say about a component *before* anyone
# downloads it — above all which capabilities it asks the host for, which is the
# whole point of the manifest it is read from.
#
# A wrapper around `cargo run --example registry_index` rather than a generator
# in its own right, and that split is the point: the fields that describe what a
# component declares are read by `jan_klod_core::ext::list`, the same reader the
# boot path uses, so the index cannot describe a manifest differently from how
# the host will enforce it. What is left here is what a shell wrapper is good
# at — resolving paths so the caller's working directory does not matter.
#
# Usage: registry-index.sh <ext-dir> <base-url> <author> <out-file>
set -eu

[ $# -eq 4 ] || {
    echo "usage: registry-index.sh <ext-dir> <base-url> <author> <out-file>" >&2
    exit 2
}

# Resolved from this script, not from the caller's cwd: `registry-index.sh` is
# run from the repository root by `make`, and from a temp directory by the
# drift check — which runs it from elsewhere on purpose, to catch a generator
# whose output depends on where it was invoked.
HERE="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(CDPATH='' cd -- "$HERE/.." && pwd)"

# Both paths are made absolute against the caller's cwd *here*, before cargo is
# involved. `cargo run` decides what working directory the example starts in,
# and this script is not the place to depend on that answer — a relative
# `ext/` that resolved differently under `make` and under the drift check would
# be a generator that works in one and not the other.
absolute() {
    case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s/%s' "$(pwd)" "$1" ;;
    esac
}

exec cargo run --quiet \
    --manifest-path "$ROOT/src/Cargo.toml" \
    -p jan-klod-core --features examples --example registry_index \
    -- "$(absolute "$1")" "$2" "$3" "$(absolute "$4")"
