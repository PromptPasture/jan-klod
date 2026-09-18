# Pinned versions of every tool whose output or CLI a Makefile target depends on.
#
# This file exists rather than a block in the root Makefile because two makefiles
# need these values: the root one, and src/extensions/Makefile, which CI invokes
# directly (`make -C src/extensions go-supply-chain`) and which therefore cannot
# inherit a variable from a parent make that never ran. A pin copied into both
# would be a pin that can drift, which is the bug this file's newest entry is
# about (#131).
#
# Each pin also has a twin in .github/workflows/ci.yml. Unpinned, CI resolves
# `@latest` while a developer machine keeps whatever it installed months ago, and
# the two disagree without either being wrong.
#
# cargo-deny is why: 0.20 removed `--config` from the `check` subcommand and made
# it global, so 0.19 and 0.20 need different invocations and no spelling
# satisfies both. CI had `@latest` (0.20.2), local machines had 0.19, and the
# supply-chain job failed on main for days. The other three carry the identical
# exposure — cargo-nextest most of all, since these Makefiles read its output —
# so they are pinned to the versions current when this was written, which are
# also the ones this repository has been verified against.
CARGO_DENY_VERSION := 0.20.2
CARGO_AUDIT_VERSION := 0.22.2
CARGO_CYCLONEDX_VERSION := 0.5.9
CARGO_NEXTEST_VERSION := 0.9.143

# A floating *scanner* is worse than a floating formatter in one specific way: a
# new release can change findings, so the gate can go red on a commit that
# changed nothing, and the fix is not obvious to whoever is holding the failing
# build. govulncheck was the one tool still on `@latest` after cargo-deny cost
# this repository days of red CI — the lesson was recorded beside the single
# ecosystem that had already learned it, and this is the other one learning it.
#
# Invoked as `go run golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION)`
# from both src/extensions/Makefile (go-supply-chain) and the root Makefile
# (supervisor-supply-chain). `go run …@version` rather than a binary off PATH
# because no runner has govulncheck installed.
GOVULNCHECK_VERSION := v1.8.0

# Not a cargo plugin, but the same rule applies: `make extensions` reads its
# output to generate each guest's capability manifest, so a change to how it
# prints a component's WIT lands on us.
WASM_TOOLS_VERSION := 1.258.0

# wkg resolves wit/spike/deps against the committed wit/wkg.lock (#77). Pinned
# for the same reason as everything else here, and to the same value CI installs
# it at (see the "Install wkg" steps in .github/workflows/ci.yml): a lockfile is
# a resolution of *some* registry state at the time it was written, and a newer
# wkg is not guaranteed to reproduce it.
WKG_VERSION := 0.15.1

# jaq is jq, in Rust, so the one tool the scripts here depended on without
# pinning becomes `cargo install`-able like the rest (#208). It reads
# `cargo metadata` for each guest's manifest, merges the two workspaces'
# CycloneDX documents, and checks the registry index is sorted — output every
# one of those is read by something else, which is this file's whole subject.
#
# **It is not a drop-in, and the difference is one flag.** `jq -s` over several
# file arguments slurps *across* them into one array; `jaq -s` slurps per file
# and emits one document each. The SBOM merge depended on the former, so it
# pipes the files in as one stream now, where jaq's output is byte-identical to
# jq's. Verified against all four filter sites before the swap, which the
# toolchain survey (#167) specifically warned not to skip.
JAQ_VERSION := 2.3.0

# jco componentizes the TypeScript guest against the same `wit/` the Rust ones
# build from (#187). Pinned for the usual reason — `make extensions` stages
# what it emits and `manifests.sh` reads that artifact's imports — and, unlike
# every other tool here, **not installed by `make setup`**: it brings 326 MB of
# `node_modules` and produces a 12.7 MB component, because every JavaScript
# component carries an engine. A developer who wants the TypeScript guest asks
# for it; everyone else gets a build that says it skipped.
JCO_VERSION := 1.34.0

# componentize-py — the Python guest's toolchain (#188). Same posture as
# `JCO_VERSION` above and for the same reason, at a different scale: 50 MB of
# toolchain producing an 18.4 MB component, because every Python component
# carries CPython. Not installed by `make setup`; a developer who wants the
# Python guest asks for it, and everyone else gets a build that says it
# skipped.
COMPONENTIZE_PY_VERSION := 0.25.1
