#!/usr/bin/env sh
# Check the generated manifests against the generator's reading rules: exports and
# type-only imports are not capabilities, and a capability is a host-* interface.
# Not a regenerate-and-diff, which would compare the generator with itself.
#
# Usage: manifests-verify.sh <ext-dir> <guest>...
set -eu

EXT="$1"
shift

fail() {
    echo "manifests-verify: $1" >&2
    exit 1
}

# The capability list of one manifest, comma-separated, in file order.
caps_of() {
    sed -n '/^capabilities = \[/,/^\]/p' "$1" |
        sed -n 's/^    "\([a-z-]*\)",$/\1/p' |
        paste -sd, -
}

for guest in "$@"; do
    manifest="$EXT/$guest.manifest.toml"
    [ -f "$manifest" ] || fail "$guest has no manifest in $EXT"

    for key in name version api-version kind description capabilities; do
        grep -q "^$key = " "$manifest" ||
            fail "$manifest has no \`$key\`"
    done

    # Anything but host-* means the import filter widened.
    for cap in $(caps_of "$manifest" | tr ',' ' '); do
        case "$cap" in
            host-*) ;;
            *) fail "$manifest lists \`$cap\`, which is not a host capability" ;;
        esac
    done
done

# Hand-checked expectations against `wasm-tools component wit`: a change in how
# imports are read must disagree with one of these.
expect() {
    actual="$(caps_of "$EXT/$1.manifest.toml")"
    [ "$actual" = "$2" ] ||
        fail "$1 declares [$actual], expected [$2] — $3"
}

expect tool-fs "host-fs" "a world's exports are not capabilities"
expect provider-openai "host-config,host-http,host-log,host-secrets" \
    "type-only imports are not capabilities"
expect tool-escape-probe "host-log" "an escape probe asks for no capability"
# The per-turn sandbox warning depends on this declaration.
expect tool-shell "host-process" "a shell tool declares host-process"

echo "verified $# manifest(s) in $EXT"
