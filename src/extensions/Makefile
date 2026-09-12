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
# The cargo artifact swaps '-' for '_' (e.g. tool-fs -> tool_fs.wasm).
GUESTS := provider-openai provider-anthropic registry-skills registry-mcp interceptor-intent-router interceptor-permission interceptor-tool-selector interceptor-context interceptor-task-router interceptor-system tool-proc-probe tool-escape-probe tool-fs tool-shell tool-edit tool-find tool-git tool-fetch tool-hello

# Shared cargo-deny policy (repo root) — the same file the host workspace uses.
DENY_CONFIG := $(ROOT)/deny.toml

# Go modules under src/extensions, discovered by mask (currently only the Slice 1a
# canary). Globbed so a new Go guest is gated without editing this Makefile.
GO_MODULES := $(wildcard */go.mod)

# Shared pure-Rust libraries: workspace members that are NOT components, so they
# are tested but never built to wasm or staged in ext/.
LIBS := guest-fs

# Guests with native (host-target) unit tests. Their WIT/component code is gated
# behind `#[cfg(target_arch = "wasm32")]`, so a host `cargo test` compiles only
# the pure logic modules (e.g. the intent router) + their `#[cfg(test)]` tests —
# no wasm target required. Add a guest here once it carries such tests.
TESTABLE_GUESTS := interceptor-intent-router interceptor-permission interceptor-context interceptor-task-router interceptor-system tool-fs tool-shell tool-edit tool-find tool-git tool-fetch

.PHONY: all stage manifests clean test audit deny go-supply-chain spike-deps spike-guest $(GUESTS)

# Build every Rust guest in one cargo invocation, not 13 serial `make` recipes —
# cargo's own scheduler compiles independent guests in parallel across cores. The
# polyglot TinyGo spike stays opt-in: `make spike-guest`.
all: manifests

# Staged components, then their manifests. `manifests` depends on this same
# recipe's output, so it is ordered after it rather than beside it.
stage:
	cargo build --release --target wasm32-wasip2 $(addprefix -p ,$(GUESTS))
	mkdir -p $(EXT_DIR)
	@for g in $(GUESTS); do \
		cp target/wasm32-wasip2/release/$$(echo $$g | tr - _).wasm $(EXT_DIR)/$$g.wasm; \
	done

# One manifest per staged component, declaring what it needs.
#
# Generated, not committed, and `ext/` is why: `git ls-files ext/` is empty and
# `ext/*.wasm` is gitignored, so the directory is build output. A committed
# manifest there would describe a component the repository does not contain —
# after a bare clone it would be a declaration about a missing file. So it is
# built beside the thing it describes and removed by `clean` with it.
manifests: stage
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $(GUESTS)
	sh $(ROOT)/scripts/manifests-verify.sh $(EXT_DIR) $(GUESTS)

# Run the guests' native unit tests (host target, no wasm). Skips guests without
# host-testable modules by only iterating TESTABLE_GUESTS; LIBS are pure Rust and
# always testable.
test:
	@for g in $(LIBS) $(TESTABLE_GUESTS); do echo "== test $$g =="; cargo test -p $$g || exit 1; done
	@echo "== manifest guards =="
	sh $(ROOT)/scripts/manifests-selftest.sh $(EXT_DIR)
	@echo "== ext-new template =="
	sh $(ROOT)/scripts/ext-new-selftest.sh

# Build a single guest and stage it in ext/ (`make tool-fs`, e.g., for
# targeted dev iteration — `all` above builds every guest at once). Rust guests
# need no cargo-component — the wasm32-wasip2 target emits a component directly
# and the `wit-bindgen` crate generates the bindings. Requires rustup (`rustup
# target add wasm32-wasip2`).
$(GUESTS):
	cargo build --release --target wasm32-wasip2 -p $@
	mkdir -p $(EXT_DIR)
	cp target/wasm32-wasip2/release/$(subst -,_,$@).wasm $(EXT_DIR)/$@.wasm
	# Its manifest too. Rebuilding one guest and leaving the old manifest
	# beside it is the only way a staged manifest can describe something the
	# component no longer is, and the next slice will believe it.
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $@


# Supply-chain gates for the Rust guests (Slice 1b gate). One shared Cargo.lock
# for the whole workspace, so a single invocation covers every guest's resolved
# dependency graph — no need to loop per guest.
audit:
	cargo audit

deny:
	cargo deny -L error --manifest-path Cargo.toml --config $(DENY_CONFIG) check

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
#
# Its spike.wasm is committed (the one exception to the ignored guest components,
# see .gitignore), so that the test asserting the polyglot claim needs neither a
# TinyGo install nor a prior build. That makes the build flags part of what lands
# in git: -no-debug -opt=z takes it from 562 KB to 75 KB, smaller than the mean
# Rust guest instead of 4x it, and cuts the host's Cranelift compile of it. The
# trade is the DWARF a guest panic would use — acceptable for a canary whose only
# assertion is one successful call across the boundary. Rebuild with `make
# spike-guest` and commit the result.
SPIKE_DIR := spike
SPIKE_WIT := $(WIT_DIR)/spike
# Overridable output path, so the drift test in polyglot.rs can rebuild through
# this one recipe instead of duplicating the tinygo invocation, and write to a
# temp dir rather than over the committed fixture. Relative paths resolve inside
# $(SPIKE_DIR); pass an absolute one from anywhere else.
SPIKE_WASM ?= spike.wasm
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 -no-debug -opt=z \
		-wit-package $(SPIKE_WIT) -wit-world spike -o $(SPIKE_WASM) .

# spike.wasm is deliberately absent here: it is a committed fixture, not a build
# output, and `clean` deleting it is what made the polyglot test depend on whether
# its sibling had run first.
clean:
	rm -rf target \
		$(addprefix $(EXT_DIR)/,$(addsuffix .wasm,$(GUESTS))) \
		$(addprefix $(EXT_DIR)/,$(addsuffix .manifest.toml,$(GUESTS)))
