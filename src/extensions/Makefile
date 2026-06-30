# Builds the first-party guest extensions to Component-Model components and stages
# the Rust ones in <repo>/ext. Works standalone (`make -C src/extensions ...`) or
# via the root Makefile. Repo-root artifacts (wit/, ext/) resolve against ROOT so
# the launch directory does not matter.
ROOT := $(abspath $(CURDIR)/../..)
EXT_DIR := $(ROOT)/ext
WIT_DIR := $(ROOT)/wit

# Rust guests, by directory name (= staged ext/<name>.wasm). Add a guest here and
# it gets `make <name>` + `make <name>-docker` + a slot in `all`/`clean` for free.
# The cargo artifact swaps '-' for '_' (e.g. store-memory -> store_memory.wasm).
GUESTS := store-memory provider-openai
DOCKER_GUESTS := $(addsuffix -docker,$(GUESTS))

# Container runtime + image for guests when the host has no rustup (Homebrew rust
# can't add the wasm target). See docs/guides/development-setup.md. Override
# CONTAINER=podman if not on docker.
CONTAINER ?= docker
RUST_IMAGE := docker.io/library/rust:slim

.PHONY: all clean spike-deps spike-guest $(GUESTS) $(DOCKER_GUESTS)

# Build every Rust guest (the polyglot TinyGo spike stays opt-in: `make spike-guest`).
all: $(GUESTS)

# Build one guest and stage it in ext/. Rust guests need no cargo-component — the
# wasm32-wasip2 target emits a component directly and the `wit-bindgen` crate
# generates the bindings. Requires rustup (`rustup target add wasm32-wasip2`).
$(GUESTS):
	cd $@ && cargo build --release --target wasm32-wasip2
	mkdir -p $(EXT_DIR)
	cp $@/target/wasm32-wasip2/release/$(subst -,_,$@).wasm $(EXT_DIR)/$@.wasm

# Same build inside a container — the fallback when the host Rust is Homebrew's (no
# rustup, so no wasm target). CARGO_TARGET_DIR=/tmp/target keeps target/ out of the
# repo tree; the repo root is mounted at /work.
$(DOCKER_GUESTS): %-docker:
	mkdir -p $(EXT_DIR)
	$(CONTAINER) run --rm -v "$(ROOT)":/work -w /work/src/extensions/$* \
		-e CARGO_TARGET_DIR=/tmp/target $(RUST_IMAGE) sh -c '\
		rustup target add wasm32-wasip2 >/dev/null && \
		cargo build --release --target wasm32-wasip2 && \
		cp /tmp/target/wasm32-wasip2/release/$(subst -,_,$*).wasm /work/ext/$*.wasm'

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
	rm -rf $(SPIKE_DIR)/spike.wasm \
		$(addsuffix /target,$(GUESTS)) \
		$(addprefix $(EXT_DIR)/,$(addsuffix .wasm,$(GUESTS)))
