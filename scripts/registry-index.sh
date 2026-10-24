#!/usr/bin/env sh
# Write index.json for a directory of staged components. A thin wrapper over
# `cargo run --example registry_index`, which reads manifests with the boot path's
# reader, so the index cannot describe a manifest differently from how it is enforced.
#
# Usage: registry-index.sh <ext-dir> <base-url> <author> <out-file>
set -eu

[ $# -eq 4 ] || {
    echo "usage: registry-index.sh <ext-dir> <base-url> <author> <out-file>" >&2
    exit 2
}

# Resolved from this script, not the caller's cwd: the drift check runs it from
# elsewhere on purpose.
HERE="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(CDPATH='' cd -- "$HERE/.." && pwd)"

# Absolute before cargo runs: cargo chooses the example's working directory.
absolute() {
    case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s/%s' "$(pwd)" "$1" ;;
    esac
}

exec cargo run --quiet \
    --manifest-path "$ROOT/Cargo.toml" \
    -p jk-wasm --features examples --example registry_index \
    -- "$(absolute "$1")" "$2" "$3" "$(absolute "$4")"
