.PHONY: wit gate spike-deps spike-guest store-memory store-memory-docker run config clean

# The WIT contracts in wit/ are canonical and carry forward. Cargo + per-language
# guest build targets land in Phase 1 (see docs/concepts/roadmap.md). The `gate`
# target reproduces the Slice 1a go/no-go check end-to-end.

CORE_DIR := src/core
SPIKE_DIR := src/extensions/spike
SPIKE_WIT := $(abspath wit/spike)
SPIKE_WASM := $(abspath $(SPIKE_DIR)/spike.wasm)
EXT_DIR := $(abspath ext)
STORE_MEMORY_DIR := src/extensions/store-memory
# Container runtime + image for guests when the host has no rustup (Homebrew rust
# can't add the wasm target). See docs/guides/development-setup.md. Override
# CONTAINER=docker if not on podman.
CONTAINER ?= podman
RUST_IMAGE := docker.io/library/rust:1-slim

# Validate the WIT contract set.
wit:
	wasm-tools component wit wit/

# Slice 1a gate (historical, reproducible): build the TinyGo spike component, then
# load and call it from the Rust host example. Prints "echo: <prompt>" on success.
# The verdict is recorded in docs/decisions/2026-06-29-extension-technologies/.
gate: spike-guest
	cd $(CORE_DIR) && cargo run --quiet --example spike_gate -p jan-klod-host -- $(SPIKE_WASM) "hello, component model"

# Resolve the spike world's WIT deps (wasi:cli and friends) into wit/spike/deps.
spike-deps:
	cd wit && wkg wit fetch --wit-dir spike

# Build the TinyGo spike guest to a Component-Model component.
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 \
		-wit-package $(SPIKE_WIT) -wit-world spike -o spike.wasm .

# Build the store-memory Rust guest to a Component-Model component and stage it
# in ext/. Rust guests need no cargo-component: the wasm32-wasip2 target emits a
# component directly, with the `wit-bindgen` crate generating the guest bindings.
# Requires rustup (`rustup target add wasm32-wasip2`) — see development-setup.md.
store-memory:
	cd $(STORE_MEMORY_DIR) && cargo build --release --target wasm32-wasip2
	mkdir -p $(EXT_DIR)
	cp $(STORE_MEMORY_DIR)/target/wasm32-wasip2/release/store_memory.wasm $(EXT_DIR)/store-memory.wasm

# Same build inside a container — the fallback when the host Rust is Homebrew's
# (no rustup, so no wasm target). Keeps the target/ dir out of the repo tree.
store-memory-docker:
	mkdir -p $(EXT_DIR)
	$(CONTAINER) run --rm -v "$(CURDIR)":/work -w /work/$(STORE_MEMORY_DIR) \
		-e CARGO_TARGET_DIR=/tmp/target $(RUST_IMAGE) sh -c '\
		rustup target add wasm32-wasip2 >/dev/null && \
		cargo build --release --target wasm32-wasip2 && \
		cp /tmp/target/wasm32-wasip2/release/store_memory.wasm /work/ext/store-memory.wasm'

# Boot the real core against the repo's jan-klod.yaml: resolve the enabled
# extensions against ext/, compile any present components, run their lifecycle,
# and print the boot plan.
run:
	cd $(CORE_DIR) && cargo run --quiet -p jan-klod-host -- $(abspath jan-klod.yaml) $(abspath ext)

# Resolve jan-klod.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE_DIR) && cargo run --quiet -p jan-klod-config --example dump -- $(abspath jan-klod.yaml)

clean:
	rm -rf bin $(SPIKE_DIR)/spike.wasm $(CORE_DIR)/target \
		$(STORE_MEMORY_DIR)/target $(EXT_DIR)/store-memory.wasm
