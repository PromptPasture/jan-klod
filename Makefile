.PHONY: wit clean

# The Go MVP source has been removed (Rust pivot — see
# docs/decisions/2026-06-29-component-model-rust/Handoff.md and
# docs/concepts/roadmap.md). Cargo + per-language guest build targets land in
# Phase 1. The WIT contracts in wit/ are canonical and carry forward.

EXT_DIR := ext

# Validate the WIT contract set.
wit:
	wasm-tools component wit wit/

clean:
	rm -rf bin $(EXT_DIR)/*.wasm
