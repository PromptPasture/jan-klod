.PHONY: build check test clippy doc audit deny clean

DENY_CONFIG := $(abspath $(CURDIR)/../../deny.toml)

build:
	cargo build

check:
	cargo check

# Unit only: the integration suite runs in the root `gate` and `harness`.
# nextest, not `cargo test`: the integration tests need one process per test.
# Doctests run separately; nextest skips them.
test:
	cargo nextest run
	cargo test --doc --workspace

# Every opt-in feature must be named, or its code is skipped silently.
clippy:
	cargo clippy --workspace --all-targets --features examples,jk-host/integration -- -D warnings

doc:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --features examples,jk-host/integration

audit:
	cargo audit -f ../../Cargo.lock

deny:
	cargo deny --manifest-path ../../Cargo.toml -L error --config $(DENY_CONFIG) check

clean:
	cargo clean
