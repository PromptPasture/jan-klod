#!/usr/bin/env sh
# List files this platform's clippy did not lint: modules gated to another OS are
# never compiled here. Advisory: always exits 0, since failing would block code
# that is correct on the platform that compiles it.
#
# Usage: unlinted-elsewhere.sh <dir>...
set -eu

WS="${*:?usage: unlinted-elsewhere.sh <dir>...}"
HERE="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$HERE" in
    darwin) THIS="macos" ;;
    linux)  THIS="linux" ;;
    *)      THIS="$HERE" ;;
esac

# Whole-file gates only; an inner #[cfg] is not worth a grep.
found=$(grep -rl '^#!\[cfg(target_os = ' $WS --include='*.rs' 2>/dev/null || true)
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
