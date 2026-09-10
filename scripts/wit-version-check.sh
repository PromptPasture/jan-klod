#!/usr/bin/env sh
# Warn when a `wit/*.wit` file changed without bumping the package version.
#
# Editing an interface without bumping `package jan-klod:interfaces@X.Y.Z`
# leaves every manifest and every component claiming a version whose meaning
# changed underneath them. Nothing else notices: the drift test on
# `manifest::API_VERSION` asserts the constant matches `wit/`, and agrees just
# as happily on a version that should have been bumped.
#
# Warning-only for now, on purpose — the package is still free to change before
# the first release, so a failure here would fire on every pre-freeze edit. The
# slice that promotes this to a failure is the one that also flips the docs.
#
# # The baseline is the whole difficulty
#
# "Changed" needs something to have changed *from*, and there is no obvious
# answer: `git tag` is empty today, so "since the last release" names nothing.
# Four candidates are tried in order, and **which one was used is printed**,
# because a check that silently compares against the wrong thing is worse than
# one that does not run:
#
#   1. the latest `v*` tag reachable from HEAD — the real baseline once there is
#      a release, and this script needs no change then;
#   2. else the merge-base with `origin/main` — a branch or PR checkout, and the
#      case that catches the actual mistake: a `.wit` edited in a change that
#      does not bump the version;
#   3. else `HEAD~1` — working directly on `main`;
#   4. else nothing usable (a depth-1 clone, a single-commit repo, or no git at
#      all): say so and exit 0.
#
# Case 4 exits 0 while this is warning-only. It is also why CI sets
# `fetch-depth: 2`: a depth-1 checkout has no parent commit, so the check would
# report nothing on every run and the job would still be green.
#
# Usage: wit-version-check.sh
set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
cd "$ROOT"

say() { printf 'wit-version-check: %s\n' "$1"; }

git rev-parse --git-dir >/dev/null 2>&1 || {
    say "no baseline (not a git repository), skipped"
    exit 0
}

BASELINE=''
DISPLAY=''
WHY=''

# 1. The nearest `v*` tag. `--abbrev=0` names the tag rather than describing the
#    distance from it, and `--match` keeps a non-release tag from being picked.
if TAG="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)"; then
    BASELINE="$TAG"
    DISPLAY="$TAG"
    WHY="latest v* tag"
fi

# 2. The merge-base with `origin/main`, when there is one and it is not HEAD
#    itself — if it is, there is nothing between them to compare.
if [ -z "$BASELINE" ] && git rev-parse --verify --quiet origin/main >/dev/null; then
    if MERGE_BASE="$(git merge-base origin/main HEAD 2>/dev/null)" \
        && [ "$MERGE_BASE" != "$(git rev-parse HEAD)" ]; then
        BASELINE="$MERGE_BASE"
        DISPLAY="$(git rev-parse --short "$MERGE_BASE")"
        WHY="merge-base with origin/main"
    fi
fi

# 3. The previous commit.
if [ -z "$BASELINE" ] && git rev-parse --verify --quiet HEAD~1 >/dev/null; then
    BASELINE="$(git rev-parse HEAD~1)"
    DISPLAY="$(git rev-parse --short HEAD~1)"
    WHY="HEAD~1"
fi

# 4. Nothing to compare against. Name the reason: "shallow" is the one a CI
#    checkout falls into, and the fix for it is a setting rather than a rebase.
if [ -z "$BASELINE" ]; then
    if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
        say "no baseline (shallow clone: no v* tag, no usable origin/main, no parent commit), skipped"
    else
        say "no baseline (no v* tag, no usable origin/main, and HEAD has no parent), skipped"
    fi
    exit 0
fi

say "baseline $DISPLAY ($WHY)"
