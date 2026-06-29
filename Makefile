.PHONY: wit gate spike-deps spike-guest run config clean

# The WIT contracts in wit/ are canonical and carry forward. Cargo + per-language
# guest build targets land in Phase 1 (see docs/concepts/roadmap.md). The `gate`
# target reproduces the Slice 1a go/no-go check end-to-end.

CORE_DIR := src/core
SPIKE_DIR := src/extensions/spike
SPIKE_WIT := $(abspath wit/spike)
SPIKE_WASM := $(abspath $(SPIKE_DIR)/spike.wasm)

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

# Boot the real core against the repo's jan-klod.yaml: resolve the enabled
# extensions against ext/, compile any present components, run their lifecycle,
# and print the boot plan.
run:
	cd $(CORE_DIR) && cargo run --quiet -p jan-klod-host -- $(abspath jan-klod.yaml) $(abspath ext)

# Resolve jan-klod.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE_DIR) && cargo run --quiet -p jan-klod-config --example dump -- $(abspath jan-klod.yaml)

clean:
	rm -rf bin $(SPIKE_DIR)/spike.wasm $(CORE_DIR)/target
