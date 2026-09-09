.PHONY: build check test clippy audit deny clean

# Builds, tests, and lints the Rust host workspace (core + config + host crates).
# Strictly core: actually running the integrated system needs the guests staged in
# ext/, which spans both subtrees — that orchestration lives in the root Makefile.

# The shared cargo-deny policy lives at the repo root (one policy for the host
# workspace and every guest). Resolve it absolutely so the target works from here.
DENY_CONFIG := $(abspath $(CURDIR)/../../deny.toml)

# Compile the host workspace without running it.
build:
	cargo build

# Type-check the host workspace without codegen or linking — a faster fail-fast
# gate than `build` for callers (e.g. pre-commit) that follow up with `test`
# anyway, which does the real build.
check:
	cargo check

# Host-side tests, under cargo-nextest. Config in .config/nextest.toml; install
# the runner with `make setup` from the repo root.
#
# nextest and not `cargo test`, for two reasons that are both about this suite
# specifically rather than a preference:
#
#  1. One process per test. The host's integration tests are one binary now
#     (host/tests/it/), and several of its modules set process-global env vars to
#     conflicting values — JK_ANSWER_TIMEOUT_SECS at 5 and at 120, two distinct
#     pairs of credential leak-canaries. Under `cargo test`'s in-binary threading
#     those race, and the canary pair races a *security* assertion into passing
#     for an unrelated reason.
#  2. A filter matching nothing is an error. `cargo test -- nonexistent` exits 0;
#     nextest exits 4. This repo's recurring bug is a check that reads green
#     without having run, so the runner refusing to be a no-op is load-bearing,
#     not a nicety.
#
# nextest does not run doctests, so they run alongside. The workspace has none
# today, which is why this costs ~0.9s — but `missing_docs` is on for every
# crate, so the first real doctest would otherwise silently go unrun.
test:
	cargo nextest run
	cargo test --doc --workspace

# The repo lint policy, enforced as CI will (-D warnings).
#
# `--features examples` because the examples are opt-in (see host/Cargo.toml) and
# this is where they stay covered. clippy type-checks without linking, so adding
# them here costs almost nothing — unlike the test targets, which used to *link*
# each example and left ~500MB in target/debug/examples for binaries no test
# runs. Lint everything, build only what is needed.
clippy:
	cargo clippy --workspace --all-targets --features examples -- -D warnings

# Supply-chain gates for the host workspace (Slice 1b gate). cargo-audit checks the
# locked tree against the RUSTSEC database; cargo-deny applies the repo-wide
# license / advisory / source policy. Both read Cargo.lock — no build required.
audit:
	cargo audit

deny:
	cargo deny -L error check --config $(DENY_CONFIG)

clean:
	cargo clean
