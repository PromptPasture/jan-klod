.PHONY: build check test test-fast clippy audit deny clean

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
# gate than `build` for callers (e.g. pre-commit) that follow up with `cargo test`
# anyway, which does the real build.
check:
	cargo check

# Host-side unit tests (config/boot + ureq status mapping).
test:
	cargo test

# The same suite under cargo-nextest: one process per test rather than one per
# binary, scheduled across all cores in a single pass. Config in .config/
# nextest.toml; install the runner with `make setup` from the repo root.
#
# Kept beside `test` rather than replacing it — the gates (`make gate`, ci.yml,
# the hooks) stay on `cargo test` for now. nextest also gives the process
# isolation that merging the integration test binaries into one will need, since
# several of those tests set conflicting process-global env vars.
#
# nextest does not run doctests. This workspace has none — all four Doc-tests
# targets report zero — so nothing is lost; if one is ever added, `cargo test
# --doc` belongs beside this.
test-fast:
	cargo nextest run

# The repo lint policy, enforced as CI will (-D warnings).
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# Supply-chain gates for the host workspace (Slice 1b gate). cargo-audit checks the
# locked tree against the RUSTSEC database; cargo-deny applies the repo-wide
# license / advisory / source policy. Both read Cargo.lock — no build required.
audit:
	cargo audit

deny:
	cargo deny -L error check --config $(DENY_CONFIG)

clean:
	cargo clean
