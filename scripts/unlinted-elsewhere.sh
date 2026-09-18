#!/usr/bin/env sh
# Name the files this platform's clippy could not have linted.
#
# A module gated to another OS is not compiled here, so clippy never reads it
# — and a lint error in it is invisible until CI. Printing the list turns
# "my gate was green" into "my gate was green, and here is what it did not
# cover", which is the difference between a trustworthy gate and a quiet one
# (#207).
#
# Advisory: always exits 0. This is not a second lint, it is a footnote on
# the one that just ran. Making it fail would block work on every platform
# for code that is correct on the platform that compiles it.
#
# Usage: unlinted-elsewhere.sh <workspace-dir>
set -eu

WS="${1:?usage: unlinted-elsewhere.sh <workspace-dir>}"
HERE="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$HERE" in
    darwin) THIS="macos" ;;
    linux)  THIS="linux" ;;
    *)      THIS="$HERE" ;;
esac

# Only whole-file gates. A `#[cfg]` on an inner item is not worth chasing with
# a grep, and the file-level ones are where whole modules go unread.
found=$(grep -rl '^#!\[cfg(target_os = ' "$WS" --include='*.rs' 2>/dev/null || true)
[ -n "$found" ] || exit 0

listed=""
for file in $found; do
    grep -q "^#!\[cfg(target_os = \"$THIS\")\]" "$file" && continue
    listed="$listed  $file
"
done
[ -n "$listed" ] || exit 0

echo "clippy: not linted on $THIS — gated to another platform, covered by CI:"
printf '%s' "$listed"
