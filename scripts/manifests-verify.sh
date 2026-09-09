#!/usr/bin/env sh
# Check the staged manifests say what they should.
#
# Deliberately *not* "regenerate and diff". These manifests are generated, not
# committed, so a diff against a fresh regeneration compares the generator with
# itself and passes whatever the generator does — including listing a world's
# exports as capabilities. What is worth asserting is that the generator's
# reading rules still hold, against values that were checked by hand once:
#
#   * exports are not capabilities (`tool-callable`, `extension-lifecycle`)
#   * type-only imports are not capabilities (`llm-types`, `store-types`)
#   * a capability is a `host-*` interface and nothing else
#
# Run after generating, so a generator that starts reading the wrong lines fails
# the build rather than quietly widening every manifest.
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

    # A capability is a host interface. Anything else means the import filter
    # widened — an export or a type-only package leaking in.
    for cap in $(caps_of "$manifest" | tr ',' ' '); do
        case "$cap" in
            host-*) ;;
            *) fail "$manifest lists \`$cap\`, which is not a host capability" ;;
        esac
    done
done

# Values checked by hand against `wasm-tools component wit` once, kept here so a
# change in how imports are read has to disagree with something. Each one is
# chosen for what it proves, not for coverage.
expect() {
    actual="$(caps_of "$EXT/$1.manifest.toml")"
    [ "$actual" = "$2" ] ||
        fail "$1 declares [$actual], expected [$2] — $3"
}

# Exports excluded: this guest exports `tool-callable` and `extension-lifecycle`
# and imports one capability. A grep over the whole WIT output lists all three.
expect tool-fs "host-fs" "a world's exports are not capabilities"
# Type-only imports excluded: this one also imports `llm-types`, which grants
# nothing and must not read as something an operator has to allow.
expect provider-openai "host-config,host-http,host-log" \
    "type-only imports are not capabilities"
# The component written to attempt escapes with plain `std` rather than typed
# imports asks for nothing but logging — the manifest should say so.
expect tool-escape-probe "host-log" "an escape probe asks for no capability"
# And the one that genuinely runs commands says so, which is the fact the
# per-turn sandbox warning is waiting on.
expect tool-shell "host-process" "a shell tool declares host-process"

echo "verified $# manifest(s) in $EXT"
