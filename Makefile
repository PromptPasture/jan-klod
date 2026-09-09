.PHONY: help wit all core extensions supervisor bundle test test-core test-guests harness gate clippy audit deny sbom supply-chain run serve chat chat-telegram probe config clean install-hooks setup

.DEFAULT_GOAL := all

# Top-level orchestration. The real work lives in two sub-makefiles:
#   src/core/Makefile       — build/test/lint the Rust host workspace (core only)
#   src/extensions/Makefile — build the guest components (staged in ext/)
# This file validates the root-level WIT contracts, builds both subtrees, and owns
# the integration targets that run the host against guests staged in ext/.

CORE := src/core
EXT := src/extensions
SUPERVISOR := src/supervisor

# Repo-root artifacts the host runs against (above any single subtree). EXT_DIR
# mirrors the staging dir the extensions sub-makefile writes to — kept in sync by
# convention (one shared constant doesn't yet justify a common include).
CONFIG := $(abspath config.yaml)
EXT_DIR := $(abspath ext)

# List the common targets.
help:
	@echo "Targets:"
	@echo "  all         build the host workspace + Rust guests (default)"
	@echo "  core        build the host workspace"
	@echo "  extensions  build the Rust guests, staged in ext/"
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

# Validate the root-level WIT contract set (canonical, language-neutral — it sits
# above any single language's code, so it stays a root concern).
wit:
	wasm-tools component wit wit/

# Build the whole project: the host workspace plus the Rust guests (staged in ext/).
all: core extensions

# --- Build (delegated to the sub-makefiles) ---
core:
	$(MAKE) -C $(CORE) build

extensions:
	$(MAKE) -C $(EXT) all

# Host-side unit tests: the core workspace plus the guests' native (host-target)
# tests (pure logic behind a wasm32 cfg-gate — e.g. the intent router).
test: test-core test-guests

# The host workspace's own tests. `gate` (cargo test --workspace, same manifest)
# supersedes this, so a caller that runs `gate` anyway wants `test-guests` alone.
test-core:
	$(MAKE) -C $(CORE) test

# The two legs `gate` does not reach: the guests' native tests and the Go
# supervisor. Split out because the push hook and CI both run `gate`, and running
# the full `test` beside it built and ran the host workspace suite twice.
test-guests:
	$(MAKE) -C $(EXT) test
	cd $(SUPERVISOR) && go vet ./... && go test ./...

# Build the tiny Go blue/green supervisor (static, dependency-free binary).
supervisor:
	cd $(SUPERVISOR) && go build ./...

# Assemble a self-contained, ready-to-run bundle (release core + staged guests +
# config + README) as dist/jan-klod-<version>-<os>-<arch>.tar.gz. Persistence and
# the REST surface are in-core, so ext/ holds only provider/interceptor/tool guests.
BUNDLE_OUT ?= $(abspath dist)
bundle: extensions
	cd $(CORE) && cargo build --release -p jan-klod-host -p jan-klod
	sh scripts/bundle.sh $(CORE)/target/release/jan-klod-gateway $(CORE)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT)

clippy:
	$(MAKE) -C $(CORE) clippy

# --- Supply-chain gates (Slice 1b gate; CI enforces all of these) ---

# cargo-audit / cargo-deny over the host workspace AND every guest crate. Each
# subtree owns its own invocation; the root just fans out to both.
audit deny:
	$(MAKE) -C $(CORE) $@
	$(MAKE) -C $(EXT) $@

# Software bill of materials for all Rust crates in the workspace, CycloneDX JSON.
# cargo-cyclonedx reads Cargo.lock — no build required. Install once with:
#   cargo install cargo-cyclonedx
sbom:
	cd src/core && cargo cyclonedx --format json --quiet
	jq -s '{bomFormat:.[0].bomFormat,specVersion:.[0].specVersion,version:1,serialNumber:.[0].serialNumber,components:[.[].components//[]|.[]]}' \
	  src/core/**/*.cdx.json > sbom.cdx.json

# The full gate: Rust license/advisory/source policy + RUSTSEC audit (host +
# guests), every Go module's verified-readonly vuln scan (guests + supervisor),
# and the SBOM.
supply-chain: deny audit sbom
	$(MAKE) -C $(EXT) go-supply-chain
	cd $(SUPERVISOR) && GOFLAGS=-mod=readonly go mod verify && govulncheck ./...

# --- Integration (host + staged extensions; spans both subtrees) ---

# Build the guests, then verify them offline through the Component Model:
#   component_harness — each guest's WIT interface + lifecycle, in isolation;
#   agent_loop        — the thin loop booted from config (Runtime::build_agent):
#                       a greeting short-circuits, a multi-step prompt runs the
#                       agentic path, both through the sandboxed provider + guests.
#   persistence       — a turn's transcript survives a full Runtime restart against
#                       the same host-side SQLite store (Phase 3 Slice 3a gate).
#   api_rest          — an external HTTP client POSTs a turn and gets the answer,
#                       driving the loop over the host-side REST surface (3b gate).
#   telegram          — a canned inbound Telegram message drives a turn and a reply
#                       is sent (Phase 4 Slice 4b), offline.
#   host_fs           — a guest writes+reads a workspace file through host-fs; an
#                       escape and a no-workspace call are denied (Phase 7 Slice 7a).
#   host_process      — a guest runs a command through host-process; a disabled
#                       runner denies (Phase 7 Slice 7b).
#   tool_fleet        — a ToolFleet dispatches a tool call by name to the matching
#                       tool-* extension (Phase 8 Slice 8a).
#   tool_wiring       — build_agent instantiates an enabled tool.* into the fleet
#                       from config + a workspace (Phase 8 Slice 8a).
# Both run against a canned host-http reply (no network, no api key) and skip any
# guest not staged, so this target stages them first.
harness: extensions
	cd $(CORE) && cargo test -p jan-klod-host --test component_harness --test agent_loop --test persistence --test api_rest --test telegram --test host_fs --test host_process --test tool_fleet --test tool_wiring

# Exit gate: the full offline integration surface, with nothing allowed to skip.
#
# This used to name the test files to run — seventeen of them, each with a note
# saying what it covered. The list is the problem. `JK_REQUIRE_GUESTS` exists
# because a skipped test reports as passing, and the enforcement applied only to
# files someone remembered to add: `storage_scope` and `test_layout` were written,
# committed, and were not in it. So the gate now runs the whole suite under the
# flag. Everything staged, nothing skipped, no list to forget.
#
# Everything is offline: canned host-http, no api key, no network.
gate: export JK_REQUIRE_GUESTS = 1
gate: extensions
	cd $(CORE) && cargo test --workspace

# Boot the real core against config.yaml: resolve enabled extensions against
# ext/, compile present components, run their lifecycle, print the boot plan.
run:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Serve the loop over the host-side REST surface (default 127.0.0.1:8787). Uses
# live host-http (real provider calls), so the enabled provider needs its api-key
# env. Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# TUI client — connects to the gateway (auto-starting it if not running).
# Override ADDR=host:port and SESSION=id.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(CORE) && cargo run --quiet -p jan-klod -- $(ADDR) $(SESSION)

# Run the Telegram bot (headless chat access, no UI client). Needs
# TELEGRAM_BOT_TOKEN in the environment and network access.
chat-telegram:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- telegram $(CONFIG) $(EXT_DIR)

# Drive a provider's full llm-provider.complete path end-to-end against a live
# OpenAI-compatible endpoint. Requires the provider's api-key env (e.g.
# OPENAI_API_KEY) and network access — makes a real, token-costing call.
probe:
	cd $(CORE) && cargo run --quiet -p jan-klod-host --example provider_probe -- $(CONFIG) $(EXT_DIR)

# Resolve config.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE) && cargo run --quiet -p jan-klod-config --example dump -- $(CONFIG)

# One-time developer setup: install cargo supply-chain plugins and wire git hooks.
setup:
	cargo install cargo-audit cargo-deny cargo-cyclonedx
	$(MAKE) install-hooks

install-hooks:
	git config core.hooksPath .github/hooks

clean:
	$(MAKE) -C $(CORE) clean
	$(MAKE) -C $(EXT) clean
