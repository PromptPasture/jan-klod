#!/usr/bin/env sh
# Print the `jan-klod:interfaces` package version declared by a `wit/` directory.
#
# One reader, for one reason: the version is the single source for every
# manifest's `api-version`, and a second parser is a second answer waiting to
# disagree with the first. This was inline in `manifests.sh` until the version
# needed reading from two places.
#
# It takes a **directory** rather than reading a fixed `wit/`, which is what
# makes the second caller possible: a past revision's `wit/` can be materialised
# (`git archive <ref> wit/ | tar -x -C <tmp>`) and read through this same code,
# so "the version then" and "the version now" cannot be computed differently.
#
# Refuses rather than guesses. Two `.wit` files declaring different versions
# have no answer to "which is the api-version", and neither does a directory
# with no declaration at all — picking one would put a number into every
# manifest that nothing in the tree actually claims.
#
# Usage: wit-version.sh <wit-dir>
set -eu

WIT="${1:?usage: wit-version.sh <wit-dir>}"

[ -d "$WIT" ] || {
    echo "wit-version.sh: $WIT is not a directory" >&2
    exit 1
}

# An unmatched glob stays literal in sh, and `sed` would then report a missing
# file — true but confusing. Say which directory was empty instead.
set -- "$WIT"/*.wit
[ -e "$1" ] || {
    echo "wit-version.sh: no *.wit files in $WIT" >&2
    exit 1
}

VERSION="$(sed -n 's/^package jan-klod:interfaces@\([^;]*\);.*/\1/p' "$@" | sort -u)"
case "$VERSION" in
    *"
"*)
        echo "wit-version.sh: $WIT/*.wit declare more than one package version:" >&2
        echo "$VERSION" >&2
        exit 1
        ;;
    "")
        echo "wit-version.sh: no 'package jan-klod:interfaces@…' found in $WIT" >&2
        exit 1
        ;;
esac

printf '%s\n' "$VERSION"
