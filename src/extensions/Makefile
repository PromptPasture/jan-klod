# Builds the first-party guest extensions to Component-Model components and stages
# the Rust ones in <repo>/ext. Works standalone (`make -C src/extensions ...`) or
# via the root Makefile. Repo-root artifacts (wit/, ext/) resolve against ROOT so
# the launch directory does not matter.
#
# The Rust guests share one Cargo workspace (this directory's Cargo.toml), so all
# builds/tests below run with `-p <guest>` from here rather than `cd`-ing into
# each guest — the workspace target dir and Cargo.lock are shared, not per-guest.
ROOT := $(abspath $(CURDIR)/../..)
EXT_DIR := $(ROOT)/ext
WIT_DIR := $(ROOT)/wit

# Rust guests, by directory name (= staged ext/<name>.wasm). Add a guest here and
# it gets `make <name>` + a slot in `all`/`clean` for free.
# The cargo artifact swaps '-' for '_' (e.g. store-memory -> store_memory.wasm).
GUESTS := store-memory provider-openai provider-anthropic registry-skills registry-mcp interceptor-intent-router interceptor-permission interceptor-tool-selector interceptor-context interceptor-task-router tool-proc-probe tool-fs tool-shell

# Shared cargo-deny policy (repo root) — the same file the host workspace uses.
DENY_CONFIG := $(ROOT)/deny.toml

# Go modules under src/extensions, discovered by mask (currently only the Slice 1a
# canary). Globbed so a new Go guest is gated without editing this Makefile.
GO_MODULES := $(wildcard */go.mod)

# Guests with native (host-target) unit tests. Their WIT/component code is gated
# behind `#[cfg(target_arch = "wasm32")]`, so a host `cargo test` compiles only
# the pure logic modules (e.g. the intent router) + their `#[cfg(test)]` tests —
# no wasm target required. Add a guest here once it carries such tests.
TESTABLE_GUESTS := interceptor-intent-router interceptor-permission interceptor-context interceptor-task-router tool-fs tool-shell

.PHONY: all clean test audit deny go-supply-chain spike-deps spike-guest $(GUESTS)

# Build every Rust guest in one cargo invocation, not 13 serial `make` recipes —
# cargo's own scheduler compiles independent guests in parallel across cores. The
# polyglot TinyGo spike stays opt-in: `make spike-guest`.
all:
	cargo build --release --target wasm32-wasip2 $(addprefix -p ,$(GUESTS))
	mkdir -p $(EXT_DIR)
	@for g in $(GUESTS); do \
		cp target/wasm32-wasip2/release/$$(echo $$g | tr - _).wasm $(EXT_DIR)/$$g.wasm; \
	done

# Run the guests' native unit tests (host target, no wasm). Skips guests without
# host-testable modules by only iterating TESTABLE_GUESTS.
test:
	@for g in $(TESTABLE_GUESTS); do echo "== test $$g =="; cargo test -p $$g || exit 1; done

# Build a single guest and stage it in ext/ (`make store-memory`, e.g., for
# targeted dev iteration — `all` above builds every guest at once). Rust guests
# need no cargo-component — the wasm32-wasip2 target emits a component directly
# and the `wit-bindgen` crate generates the bindings. Requires rustup (`rustup
# target add wasm32-wasip2`).
$(GUESTS):
	cargo build --release --target wasm32-wasip2 -p $@
	mkdir -p $(EXT_DIR)
	cp target/wasm32-wasip2/release/$(subst -,_,$@).wasm $(EXT_DIR)/$@.wasm


# Supply-chain gates for the Rust guests (Slice 1b gate). One shared Cargo.lock
# for the whole workspace, so a single invocation covers every guest's resolved
# dependency graph — no need to loop per guest.
audit:
	cargo audit

deny:
	cargo deny --manifest-path Cargo.toml check --config $(DENY_CONFIG)

# Supply-chain gate for every Go module under src/extensions (currently only the
# TinyGo Slice 1a canary): verified module sums + a govulncheck scan. GOFLAGS pins
# -mod=readonly so a scan can never silently mutate go.mod/go.sum.
go-supply-chain: export GOFLAGS = -mod=readonly
go-supply-chain:
	@for mod in $(GO_MODULES); do dir=$$(dirname $$mod); echo "== go supply-chain $$dir =="; \
		( cd $$dir && go mod verify && go run golang.org/x/vuln/cmd/govulncheck@latest ./... ) || exit 1; done

# Resolve the spike world's WIT deps (wasi:cli and friends) into wit/spike/deps.
spike-deps:
	cd $(WIT_DIR) && wkg wit fetch --wit-dir spike

# The TinyGo spike is the Slice 1a gate canary — a one-off, not a real guest, so it
# stays separate from GUESTS and out of `all`.
SPIKE_DIR := spike
SPIKE_WIT := $(WIT_DIR)/spike
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 \
		-wit-package $(SPIKE_WIT) -wit-world spike -o spike.wasm .

clean:
	rm -rf $(SPIKE_DIR)/spike.wasm target \
		$(addprefix $(EXT_DIR)/,$(addsuffix .wasm,$(GUESTS)))
