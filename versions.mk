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
