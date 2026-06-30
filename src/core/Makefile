.PHONY: build test clippy clean

# Builds, tests, and lints the Rust host workspace (core + config + host crates).
# Strictly core: actually running the integrated system needs the guests staged in
# ext/, which spans both subtrees — that orchestration lives in the root Makefile.

# Compile the host workspace without running it.
build:
	cargo build

# Host-side unit tests (config/boot + ureq status mapping).
test:
	cargo test

# The repo lint policy, enforced as CI will (-D warnings).
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

clean:
	cargo clean
