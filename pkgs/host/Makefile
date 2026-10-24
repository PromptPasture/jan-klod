.PHONY: build check test clippy audit deny clean

# Builds, tests, and lints the host cargo workspace. Running the integrated system
# (guests staged in ext/) is orchestrated by the root Makefile.

# Shared cargo-deny policy at the repo root.
DENY_CONFIG := $(abspath $(CURDIR)/../../deny.toml)

build:
	cargo build

# Type-check only: a faster pre-commit gate than `build`, since `test` does the real build.
check:
	cargo check

# Unit tests under cargo-nextest (config: .config/nextest.toml; `make setup` installs it).
#
# Unit only: the integration suite needs the `integration` feature (#79) and runs in the
# root Makefile's `gate` and `harness`. Keeps pre-commit free of the wasmtime link.
#
# nextest, not `cargo test`: it runs one process per test, which the integration tests need
# (they set conflicting process-global env vars), and a filter matching nothing exits 4.
#
# Doctests run alongside, since nextest skips them. `missing_docs` is on for every crate,
# so the first doctest would otherwise go unrun.
test:
	cargo nextest run
	cargo test --doc --workspace

# Lints as CI does. Every opt-in feature must be named here, or its code is skipped silently.
clippy:
	cargo clippy --workspace --all-targets --features examples,jk-host/integration -- -D warnings

# Supply-chain gates. Both read Cargo.lock; no build required.
audit:
	cargo audit -f ../../Cargo.lock

deny:
	cargo deny --manifest-path ../../Cargo.toml -L error --config $(DENY_CONFIG) check

clean:
	cargo clean
