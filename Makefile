.PHONY: help wit all core extensions test harness clippy run probe config clean

.DEFAULT_GOAL := all

# Top-level orchestration. The real work lives in two sub-makefiles:
#   src/core/Makefile       — build/test/lint the Rust host workspace (core only)
#   src/extensions/Makefile — build the guest components (staged in ext/)
# This file validates the root-level WIT contracts, builds both subtrees, and owns
# the integration targets that run the host against guests staged in ext/.

CORE := src/core
EXT := src/extensions

# Repo-root artifacts the host runs against (above any single subtree). EXT_DIR
# mirrors the staging dir the extensions sub-makefile writes to — kept in sync by
# convention (one shared constant doesn't yet justify a common include).
CONFIG := $(abspath jan-klod.yaml)
EXT_DIR := $(abspath ext)

# List the common targets.
help:
	@echo "Targets:"
	@echo "  all         build the host workspace + Rust guests (default)"
	@echo "  core        build the host workspace"
	@echo "  extensions  build the Rust guests, staged in ext/"
	@echo "  test        run host-side unit tests"
	@echo "  harness     build guests, then verify each through the component harness"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  run         boot the core against jan-klod.yaml + ext/"
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

test clippy:
	$(MAKE) -C $(CORE) $@

# --- Integration (host + staged extensions; spans both subtrees) ---

# Build the guests, then drive each through the component harness: load it, wire
# host capabilities, and verify its WIT interface + lifecycle offline (the
# provider runs against a canned host-http reply — no network, no api key). The
# harness itself skips any guest not staged, so this target stages them first.
harness: extensions
	cd $(CORE) && cargo test -p jan-klod-host --test component_harness

# Boot the real core against jan-klod.yaml: resolve enabled extensions against
# ext/, compile present components, run their lifecycle, print the boot plan.
run:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Drive a provider's full llm-provider.complete path end-to-end against a live
# OpenAI-compatible endpoint. Requires the provider's api-key env (e.g.
# OPENAI_API_KEY) and network access — makes a real, token-costing call.
probe:
	cd $(CORE) && cargo run --quiet -p jan-klod-host --example provider_probe -- $(CONFIG) $(EXT_DIR)

# Resolve jan-klod.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE) && cargo run --quiet -p jan-klod-config --example dump -- $(CONFIG)

# Clean both subtrees.
clean:
	$(MAKE) -C $(CORE) clean
	$(MAKE) -C $(EXT) clean
