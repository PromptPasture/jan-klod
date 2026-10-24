#!/usr/bin/env sh
# Print the `jan-klod:interfaces` version declared by a wit/ directory: the single
# source for every manifest's api-version. Takes a directory, so a past revision
# (`git archive <ref> wit/ | tar -x -C <tmp>`) reads through the same code.
# Refuses when files disagree or none declares it, rather than guessing.
#
# Usage: wit-version.sh <wit-dir>
set -eu

WIT="${1:?usage: wit-version.sh <wit-dir>}"

[ -d "$WIT" ] || {
    echo "wit-version.sh: $WIT is not a directory" >&2
    exit 1
}

# An unmatched glob stays literal; name the directory instead of sed's error.
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
