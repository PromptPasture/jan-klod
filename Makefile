.PHONY: help wit all core extensions supervisor bundle test harness phase2-gate phase3-gate phase4-gate clippy audit deny sbom supply-chain run serve chat chat-telegram probe config clean

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
	@echo "  test        run host-side unit tests"
	@echo "  harness     build guests, then verify each + the exit-gate flow offline"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  supply-chain  run every supply-chain gate (audit + deny + sbom + go)"
	@echo "  audit       cargo-audit the host workspace + every guest (RUSTSEC)"
	@echo "  deny        cargo-deny license/advisory/source policy (host + guests)"
	@echo "  sbom        generate sbom.spdx.json for the repo (syft)"
	@echo "  run         boot the core against config.yaml + ext/"
	@echo "  probe       drive a live provider completion (needs api key + network)"
	@echo "  config      print the resolved extension plan"
	@echo "  wit         validate the WIT contracts"
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
test:
	$(MAKE) -C $(CORE) test
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
	cd $(CORE) && cargo build --release -p jan-klod-host
	sh scripts/bundle.sh $(CORE)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT)

clippy:
	$(MAKE) -C $(CORE) clippy

# --- Supply-chain gates (Slice 1b gate; CI enforces all of these) ---

# cargo-audit / cargo-deny over the host workspace AND every guest crate. Each
# subtree owns its own invocation; the root just fans out to both.
audit deny:
	$(MAKE) -C $(CORE) $@
	$(MAKE) -C $(EXT) $@

# Software bill of materials for the whole deploy unit, SPDX-JSON. syft reads the
# committed lockfiles (Cargo.lock, go.sum) — no build required.
sbom:
	syft dir:. --source-name jan-klod -o spdx-json=sbom.spdx.json

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
# Both run against a canned host-http reply (no network, no api key) and skip any
# guest not staged, so this target stages them first.
harness: extensions
	cd $(CORE) && cargo test -p jan-klod-host --test component_harness --test agent_loop --test persistence --test api_rest --test telegram

# Phase 2 exit gate: boot the real core from a config with two providers, a routing
# table, and all v1 interceptors enabled, and run the full thin loop offline —
# intent -> shaping -> completion with provider fallback -> grounded answer. Stages
# the guests first; skips if any is not built.
phase2-gate: extensions
	cd $(CORE) && cargo test -p jan-klod-host --test phase2_gate

# Phase 3 exit gate: durable state survives a Runtime restart (persistence) AND an
# external HTTP client drives the loop over the host-side REST surface (api_rest),
# both offline. Stages the guests first.
phase3-gate: extensions
	cd $(CORE) && cargo test -p jan-klod-host --test persistence --test api_rest

# Phase 4 exit gate: a UI client drives core over REST (jan-klod-ui roundtrip) AND
# an inbound Telegram message drives a turn and a reply (telegram), both offline.
phase4-gate: extensions
	cd $(CORE) && cargo test -p jan-klod-ui --test roundtrip
	cd $(CORE) && cargo test -p jan-klod-host --test telegram

# Boot the real core against config.yaml: resolve enabled extensions against
# ext/, compile present components, run their lifecycle, print the boot plan.
run:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Serve the loop over the host-side REST surface (default 127.0.0.1:8787). Uses
# live host-http (real provider calls), so the enabled provider needs its api-key
# env. POST {"session":"…","message":"…"} to drive a turn. Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# REPL client for a running `jan-klod serve` — a separate client process that drives
# core over the REST surface. Override ADDR=host:port and SESSION=id.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(CORE) && cargo run --quiet -p jan-klod-ui -- $(ADDR) $(SESSION)

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

# Clean both subtrees.
clean:
	$(MAKE) -C $(CORE) clean
	$(MAKE) -C $(EXT) clean
