#!/bin/sh
# Drift check for the committed src/web/dist/.
#
# #119 box 1 chose to commit the bundle, and the argument it recorded was that
# this repository already commits generated artifacts — protocol.schema.json,
# ext/*.manifest.toml, wit/wkg.lock — each with a drift check. dist/ was the
# first one without, and src/core/host/src/serve.rs embeds it with include_str!,
# so the failure mode is specific and silent: edit src/web/src/*.ts, do not
# rebuild, and the core serves a stale client forever with everything green.
#
# Same shape as ext-new-selftest.sh and protocol.schema.json's test: rebuild,
# compare, fail on a difference. Rebuilt into a temp tree rather than over the
# committed dist/, so a run leaves the working tree exactly as it found it and
# a failure is a report rather than a half-applied fix.
#
# `npm ci`, never `npm install`: esbuild's minified output is not stable across
# releases, so package.json's unranged "esbuild": "0.25.12" and the
# integrity-pinned lockfile are what make "rebuild and compare" a check on the
# sources rather than on whichever esbuild happened to resolve. #127 box 1
# measured the rest — two in-place rebuilds and one from a different absolute
# path produced identical bytes, and the output contains no path or timestamp —
# so nothing here needs normalising before the diff.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WEB="$ROOT/src/web"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "web-dist-drift: rebuilding src/web from its sources in a temp tree"

# node_modules deliberately not copied: `npm ci` below installs from the
# lockfile, which is the whole point. tests/ is not copied either — tsconfig.json
# does not need it to type-check src/, and the suite is `make test-web`'s job.
cp -R "$WEB/src" "$WEB/package.json" "$WEB/package-lock.json" "$WEB/tsconfig.json" "$WORK/"
mkdir -p "$WORK/dist"
(cd "$WORK" && npm ci --silent && npm run build >/dev/null)

for file in app.js index.html; do
    if ! diff -u "$WEB/dist/$file" "$WORK/dist/$file"; then
        echo "web-dist-drift: src/web/dist/$file has drifted from src/web/src/" >&2
        echo "  serve.rs embeds dist/ with include_str!, so a stale bundle ships silently." >&2
        echo "  Rebuild it rather than editing it by hand:" >&2
        echo "    cd src/web && npm ci && npm run build && git add dist" >&2
        exit 1
    fi
done

echo "web-dist-drift: dist/ is what src/ builds, 0 drift"
