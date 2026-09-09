.PHONY: help wit all core extensions ext supervisor bundle test test-core test-guests harness gate clippy audit deny sbom supply-chain run serve chat chat-telegram probe config clean install-hooks setup

.DEFAULT_GOAL := all

# Orchestrates two sub-makefiles: src/core/Makefile (host workspace) and
# src/extensions/Makefile (guest components, staged in ext/). Also owns the
# root WIT contracts and the integration targets spanning both subtrees.

CORE := src/core
EXT := src/extensions
SUPERVISOR := src/supervisor

# EXT_DIR mirrors the extensions sub-makefile's staging dir.
CONFIG := $(abspath config.yaml)
EXT_DIR := $(abspath ext)

# List the common targets.
help:
	@echo "Targets:"
	@echo "  all         build the host workspace + Rust guests (default)"
	@echo "  core        build the host workspace"
	@echo "  extensions  build the Rust guests, staged in ext/ (alias: ext)"
	@echo "  test        run host-side unit tests (core + guests + supervisor)"
	@echo "  test-core   run the host workspace's tests only"
	@echo "  test-guests run the guests' native tests + the Go supervisor only"
	@echo "  harness     build guests, then verify each + the exit-gate flow offline"
	@echo "  gate        build guests, then run the full offline integration exit gate"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  supply-chain  run every supply-chain gate (audit + deny + sbom + go)"
	@echo "  audit       cargo-audit the host workspace + every guest (RUSTSEC)"
	@echo "  deny        cargo-deny license/advisory/source policy (host + guests)"
	@echo "  sbom        generate sbom.cdx.json for Rust workspace (cargo-cyclonedx)"
	@echo "  run         boot the core against config.yaml + ext/"
	@echo "  probe       drive a live provider completion (needs api key + network)"
	@echo "  config      print the resolved extension plan"
	@echo "  wit         validate the WIT contracts"
	@echo "  setup       install cargo plugins + configure git hooks"
	@echo "  install-hooks  configure git to use .github/hooks/"
	@echo "  clean       remove build artifacts"

# Root-level WIT contracts are language-neutral, so they stay a root concern.
wit:
	wasm-tools component wit wit/

all: core extensions

# --- Build (delegated to the sub-makefiles) ---
core:
	$(MAKE) -C $(CORE) build

extensions:
	$(MAKE) -C $(EXT) all

# Alias: `ext` is the name every skip notice tells developers to run, but
# `ext/` is also a directory, so plain make no-ops on it without .PHONY.
ext: extensions

# Host-side unit tests: the core workspace plus the guests' native tests.
test: test-core test-guests

test-core:
	$(MAKE) -C $(CORE) test

# The legs `gate` doesn't cover: guest native tests + the Go supervisor.
test-guests:
	$(MAKE) -C $(EXT) test
	cd $(SUPERVISOR) && go vet ./... && go test ./...

# Tiny Go blue/green supervisor (static, dependency-free binary).
supervisor:
	cd $(SUPERVISOR) && go build ./...

# Self-contained release bundle: core binary + staged guests + config + README,
# as dist/jan-klod-<version>-<os>-<arch>.tar.gz.
BUNDLE_OUT ?= $(abspath dist)
bundle: extensions
	cd $(CORE) && cargo build --release -p jan-klod-host -p jan-klod
	sh scripts/bundle.sh $(CORE)/target/release/jan-klod-gateway $(CORE)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT)

clippy:
	$(MAKE) -C $(CORE) clippy

# --- Supply-chain gates (CI enforces all of these) ---

# Each subtree owns its own audit/deny invocation; root fans out to both.
audit deny:
	$(MAKE) -C $(CORE) $@
	$(MAKE) -C $(EXT) $@

# CycloneDX SBOM for all Rust crates. Install once: cargo install cargo-cyclonedx
sbom:
	cd src/core && cargo cyclonedx --format json --quiet
	jq -s '{bomFormat:.[0].bomFormat,specVersion:.[0].specVersion,version:1,serialNumber:.[0].serialNumber,components:[.[].components//[]|.[]]}' \
	  src/core/**/*.cdx.json > sbom.cdx.json

# License/advisory/source policy + RUSTSEC audit (host + guests), Go vuln
# scan (guests + supervisor), and the SBOM.
supply-chain: deny audit sbom
	$(MAKE) -C $(EXT) go-supply-chain
	cd $(SUPERVISOR) && GOFLAGS=-mod=readonly go mod verify && govulncheck ./...

# --- Integration (host + staged extensions; spans both subtrees) ---

# Build guests, then verify each through the Component Model, offline (canned
# host-http, no api key, no network):
#   component_harness — each guest's WIT interface + lifecycle, in isolation
#   agent_loop         — the config-driven agent loop (greeting + multi-step)
#   persistence        — a transcript survives a Runtime restart (SQLite store)
#   api_rest           — a turn driven over the host-side REST surface
#   telegram           — a canned inbound message drives a turn + reply
#   host_fs            — guest read/write through host-fs; escapes denied
#   host_process       — guest command through host-process; denied when disabled
#   tool_fleet         — ToolFleet dispatches a call to the matching tool-* guest
#   tool_wiring        — build_agent wires an enabled tool.* into the fleet
# These are modules of one test binary (src/core/host/tests/it/main.rs), run by
# --test name filter. A typo'd filter can still match other modules and exit 0,
# so each name is checked against the filesystem before the run.
HARNESS_MODULES := component_harness agent_loop persistence api_rest rpc telegram \
                   host_fs host_process tool_fleet tool_wiring
harness: export JK_REQUIRE_GUESTS = 1
harness: extensions
	@for m in $(HARNESS_MODULES); do \
	  test -f $(CORE)/host/tests/it/$$m.rs \
	    || { echo "harness: no module $$m.rs in $(CORE)/host/tests/it/" >&2; exit 1; }; \
	done
	cd $(CORE) && cargo nextest run -p jan-klod-host $(addsuffix ::,$(HARNESS_MODULES))

# Exit gate: the full offline integration suite, nothing allowed to skip.
# JK_REQUIRE_GUESTS turns a silently-skipped test into a failure.
#
# nextest over `cargo test`: (1) one process per test — some modules set
# conflicting process-global env vars, which race under in-binary threading;
# (2) nextest exits nonzero on zero tests matched, `cargo test` exits 0.
# --no-fail-fast reports every failure in the run, not just the first.
# Doctests run separately since nextest doesn't run them.
gate: export JK_REQUIRE_GUESTS = 1
gate: extensions
	cd $(CORE) && cargo nextest run --workspace --no-fail-fast
	cd $(CORE) && cargo test --doc --workspace

# Boot the real core against config.yaml: resolve extensions against ext/,
# compile present components, run their lifecycle, print the boot plan.
run:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Serve the loop over the host-side REST surface. Uses live host-http, so the
# enabled provider needs its api-key env. Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# TUI client — connects to the gateway (auto-starting it if not running).
# Override ADDR=host:port and SESSION=id.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(CORE) && cargo run --quiet -p jan-klod -- $(ADDR) $(SESSION)

# Telegram bot (headless). Needs TELEGRAM_BOT_TOKEN + network.
chat-telegram:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- telegram $(CONFIG) $(EXT_DIR)

# Drives a provider's llm-provider.complete path against a live endpoint.
# Needs the provider's api-key env (e.g. OPENAI_API_KEY) — real, billed call.
probe:
	cd $(CORE) && cargo run --quiet -p jan-klod-host --features examples --example provider_probe -- $(CONFIG) $(EXT_DIR)

# Resolve config.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE) && cargo run --quiet -p jan-klod-config --features examples --example dump -- $(CONFIG)

# Every tool whose output or CLI these targets depend on is pinned, and each pin
# has a twin in .github/workflows/ci.yml. Unpinned, CI resolves `@latest` while a
# developer machine keeps whatever it installed months ago, and the two disagree
# without either being wrong.
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
# Not a cargo plugin, but the same rule applies: `make extensions` reads its
# output to generate each guest's capability manifest, so a change to how it
# prints a component's WIT lands on us.
WASM_TOOLS_VERSION := 1.258.0

# One-time developer setup: cargo supply-chain plugins + git hooks.
setup:
	cargo install cargo-audit --version $(CARGO_AUDIT_VERSION)
	cargo install cargo-cyclonedx --version $(CARGO_CYCLONEDX_VERSION)
	cargo install cargo-nextest --version $(CARGO_NEXTEST_VERSION)
	cargo install cargo-deny --version $(CARGO_DENY_VERSION)
	cargo install wasm-tools --version $(WASM_TOOLS_VERSION)
	$(MAKE) install-hooks

install-hooks:
	git config core.hooksPath .github/hooks

clean:
	$(MAKE) -C $(CORE) clean
	$(MAKE) -C $(EXT) clean
