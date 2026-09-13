.PHONY: build release test clippy fmt audit deny clean stage

# Builds, tests and lints the GUI shell — a Tauri window around the web client
# the core already serves. Its own workspace, not a member of the host workspace: see the
# comment at the top of Cargo.toml, and #141 for the measurement behind it.

# The shared cargo-deny policy lives at the repo root — one policy for all three
# workspaces. Resolve it absolutely so the target works from here.
DENY_CONFIG := $(abspath $(CURDIR)/../../deny.toml)

# Where `stage` puts the binary. `jan-klod --gui` resolves `jan-klod-gui` as a
# sibling of itself (jan_klod::sibling_bin), which a release bundle satisfies by
# construction and a developer tree does not — two workspaces means two
# `target/` directories. Copying it next to the host workspace's binaries is
# what makes `make gui && make chat-gui` work without a second rule for how the
# shell is found.
CORE_DEBUG := $(abspath $(CURDIR)/../core/target/debug)

build:
	cargo build

release:
	cargo build --release

# Unit tests (argument parsing, the token script) plus the smoke suite, which
# starts the real binary and asks a real webview what it loaded.
#
# JK_REQUIRE_GUI is deliberately *not* set here: a developer on a headless box
# should get a skip rather than a failure. CI sets it on the jobs that do have a
# display, which is where a silent skip would actually be a lie.
test:
	cargo test

clippy:
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt --all -- --check

# Supply-chain gates, same two as every other workspace here and against the
# same policy. This is the tree that made `deny.toml` grow eleven entries, so if
# any workspace must not skip these, it is this one.
audit:
	cargo audit

deny:
	cargo deny -L error --config $(DENY_CONFIG) check

stage: build
	@mkdir -p $(CORE_DEBUG)
	cp target/debug/jan-klod-gui $(CORE_DEBUG)/jan-klod-gui
	@echo "gui: staged $(CORE_DEBUG)/jan-klod-gui"

clean:
	cargo clean
