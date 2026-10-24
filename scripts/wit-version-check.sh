#!/usr/bin/env sh
# Warn when wit/*.wit changed without bumping `package jan-klod:interfaces@`.
# Warning-only: the package may still change freely before the first release.
#
# Baseline, tried in order and printed (a silent wrong baseline is worse than none):
# latest v* tag, merge-base with origin/main, HEAD~1; else skip. CI needs
# `fetch-depth: 2`, or every run skips and stays green.
#
# Usage: wit-version-check.sh
set -eu

HERE="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
ROOT="$(CDPATH='' cd -- "$HERE/.." && pwd)"
cd "$ROOT"

say() { printf 'wit-version-check: %s\n' "$1"; }

git rev-parse --git-dir >/dev/null 2>&1 || {
    say "no baseline (not a git repository), skipped"
    exit 0
}

BASELINE=''
DISPLAY=''
WHY=''

# --abbrev=0 names the tag itself; --match skips non-release tags.
if TAG="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)"; then
    BASELINE="$TAG"
    DISPLAY="$TAG"
    WHY="latest v* tag"
fi

# Skip the merge-base when it is HEAD: nothing between them to compare.
if [ -z "$BASELINE" ] && git rev-parse --verify --quiet origin/main >/dev/null; then
    if MERGE_BASE="$(git merge-base origin/main HEAD 2>/dev/null)" \
        && [ "$MERGE_BASE" != "$(git rev-parse HEAD)" ]; then
        BASELINE="$MERGE_BASE"
        DISPLAY="$(git rev-parse --short "$MERGE_BASE")"
        WHY="merge-base with origin/main"
    fi
fi

if [ -z "$BASELINE" ] && git rev-parse --verify --quiet HEAD~1 >/dev/null; then
    BASELINE="$(git rev-parse HEAD~1)"
    DISPLAY="$(git rev-parse --short HEAD~1)"
    WHY="HEAD~1"
fi

# Name the reason: a shallow CI checkout is the usual case.
if [ -z "$BASELINE" ]; then
    if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
        say "no baseline (shallow clone: no v* tag, no usable origin/main, no parent commit), skipped"
    else
        say "no baseline (no v* tag, no usable origin/main, and HEAD has no parent), skipped"
    fi
    exit 0
fi

say "baseline $DISPLAY ($WHY)"

# Against the working tree, not HEAD: an uncommitted edit with no bump is the mistake.
CHANGED="$(git diff --name-only "$BASELINE" -- wit 2>/dev/null)" || {
    say "baseline $DISPLAY cannot be read (a shallow clone may not have it), skipped"
    exit 0
}

if [ -z "$CHANGED" ]; then
    say "wit/ unchanged since $DISPLAY"
    exit 0
fi

# Read both versions with wit-version.sh, extracting the baseline's wit/ via git archive.
WORK="${TMPDIR:-/tmp}/jk-wit-version-check-$$"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
mkdir -p "$WORK"

if ! git archive "$BASELINE" wit >"$WORK/wit.tar" 2>/dev/null; then
    say "wit/ changed, but $DISPLAY has no wit/ to compare against — no version to bump"
    exit 0
fi
tar -x -f "$WORK/wit.tar" -C "$WORK"

# manifests.sh already fails the build on an unreadable version; skip here rather
# than report a mismatch that was never established.
BEFORE="$(sh "$HERE/wit-version.sh" "$WORK/wit")" || {
    say "cannot read the package version at $DISPLAY (see above), skipped"
    exit 0
}
AFTER="$(sh "$HERE/wit-version.sh" wit)" || {
    say "cannot read the package version in wit/ (see above), skipped"
    exit 0
}

if [ "$BEFORE" != "$AFTER" ]; then
    say "wit/ changed since $DISPLAY and the package version moved $BEFORE -> $AFTER"
    exit 0
fi

say "warning: wit/ changed since $DISPLAY but the package version is still $AFTER"
echo "$CHANGED" | sed 's/^/  /'
say "bump 'package jan-klod:interfaces@…' in wit/*.wit if the meaning of an interface changed"
exit 0
