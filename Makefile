.PHONY: wit gate spike-deps spike-guest host clean

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

# Slice 1a gate: fetch WIT deps, build the TinyGo spike component, then load and
# call it from the Rust host. Prints "echo: <prompt>" on success.
gate: spike-guest host

# Resolve the spike world's WIT deps (wasi:cli and friends) into wit/spike/deps.
spike-deps:
	cd wit && wkg wit fetch --wit-dir spike

# Build the TinyGo spike guest to a Component-Model component.
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 \
		-wit-package $(SPIKE_WIT) -wit-world spike -o spike.wasm .

# Build the Rust host and run it against the spike component.
host: spike-guest
	cd $(CORE_DIR) && cargo run --quiet -- $(SPIKE_WASM) "hello, component model"

clean:
	rm -rf bin $(SPIKE_DIR)/spike.wasm $(CORE_DIR)/target
