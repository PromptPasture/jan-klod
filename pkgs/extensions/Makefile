ROOT := $(abspath $(CURDIR)/../..)
EXT_DIR := $(ROOT)/ext
WIT_DIR := $(ROOT)/wit

# Included via $(ROOT) because CI invokes this makefile directly.
include $(ROOT)/versions.mk

GUESTS := host-secrets-probe interceptor-context interceptor-guardrails interceptor-intent-router interceptor-permission interceptor-persona interceptor-principal-probe interceptor-system interceptor-task-router interceptor-tool-selector provider-anthropic provider-openai registry-mcp registry-skills tool-ask-probe tool-edit tool-escape-probe tool-fetch tool-find tool-fs tool-git tool-hello tool-memory tool-plan tool-proc tool-proc-probe tool-shell tool-web-search

DENY_CONFIG := $(ROOT)/deny.toml

GO_MODULES := $(wildcard */go.mod)

LIBS := guest-fs

# Add a guest here once it has host-target unit tests.
TESTABLE_GUESTS := interceptor-context interceptor-guardrails interceptor-intent-router interceptor-permission interceptor-system interceptor-task-router interceptor-tool-selector test-interceptor-persona tool-edit tool-fetch tool-find tool-fs tool-git tool-memory tool-plan tool-shell tool-web-search

.PHONY: all stage manifests clean test clippy audit deny go-supply-chain spike-deps spike-guest ts-guest py-guest $(GUESTS)

# ts-guest is not opt-in: it skips without jco, so opting in would leave it untested.
all: manifests ts-guest py-guest

stage:
	cargo build --release --target wasm32-wasip2 $(addprefix -p ,$(GUESTS))
	mkdir -p $(EXT_DIR)
	@for g in $(GUESTS); do \
		cp target/wasm32-wasip2/release/$$(echo $$g | tr - _).wasm $(EXT_DIR)/$$g.wasm; \
	done

# Generated, not committed: a committed manifest would describe a component the repo does not contain.
manifests: stage
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $(GUESTS)
	sh $(ROOT)/scripts/manifests-verify.sh $(EXT_DIR) $(GUESTS)

test:
	@for g in $(LIBS) $(TESTABLE_GUESTS); do echo "== test $$g =="; cargo test -p $$g || exit 1; done
	@echo "== manifest guards =="
	sh $(ROOT)/scripts/manifests-selftest.sh $(EXT_DIR)
	@echo "== ext-new template =="
	sh $(ROOT)/scripts/ext-new-selftest.sh

# Skips with a message when jco or esbuild is missing.
# `--disable http --disable fetch-event` is required: otherwise the component imports wasi:http, which the host refuses.
TS_GUESTS := tool-hello-ts
ESBUILD := $(CURDIR)/node_modules/.bin/esbuild

.PHONY: ts-guest
# One shell block: a guard's exit 0 would only end its own recipe line.
ts-guest:
	@if ! command -v jco >/dev/null 2>&1; then \
	  echo "skipping $(TS_GUESTS): jco is not on PATH"; \
	  echo "  install it with: npm install -g @bytecodealliance/jco@$(JCO_VERSION)"; \
	elif [ ! -x $(ESBUILD) ]; then \
	  echo "skipping $(TS_GUESTS): esbuild is not installed"; \
	  echo "  install it with: cd pkgs/extensions && npm ci --ignore-scripts"; \
	else \
	  set -e; \
	  mkdir -p $(EXT_DIR); \
	  for g in $(TS_GUESTS); do \
	    case $$g in \
	      tool-*) world=tool-world ;; \
	      interceptor-*) world=interceptor-world ;; \
	      *) echo "ts-guest: $$g has no world for its prefix" >&2; exit 1 ;; \
	    esac; \
	    mkdir -p $$g/build; \
	    $(ESBUILD) $$g/src/guest.ts --bundle --format=esm \
	      --target=es2022 --outfile=$$g/build/guest.js; \
	    jco componentize $$g/build/guest.js --wit $(WIT_DIR) \
	      --world-name $$world --disable http --disable fetch-event \
	      -o $(EXT_DIR)/$$g.wasm; \
	    sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(WIT_DIR) $$g; \
	  done; \
	fi

# `bindings` runs before `componentize`, and the generated wit_world is removed first: `bindings` refuses to overwrite it.
# No `--disable` flags exist: CPython initialises wasi:filesystem, wasi:sockets and wasi:cli; sandbox_boundary.rs tests each refusal.
PY_GUESTS := tool-hello-py

.PHONY: py-guest
py-guest:
	@if ! command -v componentize-py >/dev/null 2>&1; then \
	  echo "skipping $(PY_GUESTS): componentize-py is not on PATH"; \
	  echo "  install it with: pipx install componentize-py==$(COMPONENTIZE_PY_VERSION)"; \
	else \
	  set -e; \
	  mkdir -p $(EXT_DIR); \
	  for g in $(PY_GUESTS); do \
	    case $$g in \
	      tool-*) world=tool-world ;; \
	      interceptor-*) world=interceptor-world ;; \
	      *) echo "py-guest: $$g has no world for its prefix" >&2; exit 1 ;; \
	    esac; \
	    rm -rf $$g/wit_world $$g/componentize_py_async_support; \
	    componentize-py --wit-path $(WIT_DIR) --world $$world bindings $$g; \
	    (cd $$g && componentize-py --wit-path $(WIT_DIR) --world $$world \
	      componentize app -o $(EXT_DIR)/$$g.wasm); \
	    sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(WIT_DIR) $$g; \
	  done; \
	fi

# Lint twice: only the wasm run sees the cfg-gated component glue.
clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --target wasm32-wasip2 -- -D warnings

# Needs `rustup target add wasm32-wasip2`.
$(GUESTS):
	cargo build --release --target wasm32-wasip2 -p $@
	mkdir -p $(EXT_DIR)
	cp target/wasm32-wasip2/release/$(subst -,_,$@).wasm $(EXT_DIR)/$@.wasm
	# Regenerate the manifest: a stale one goes unnoticed.
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $@

audit:
	cargo audit

deny:
	cargo deny -L error --manifest-path Cargo.toml --config $(DENY_CONFIG) check

# -mod=readonly stops the scan rewriting go.mod or go.sum.
go-supply-chain: export GOFLAGS = -mod=readonly
go-supply-chain:
	@for mod in $(GO_MODULES); do dir=$$(dirname $$mod); echo "== go supply-chain $$dir =="; \
		( cd $$dir && go mod verify && go run golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION) ./... ) || exit 1; done

spike-deps:
	cd $(WIT_DIR) && wkg wit fetch --wit-dir spike

# TinyGo spike canary, kept out of GUESTS and `all`.
# spike.wasm is a committed fixture; rebuild with `make spike-guest` and commit.
SPIKE_DIR := spike
SPIKE_WIT := $(WIT_DIR)/spike
# Overridable so the polyglot drift test can rebuild into a temp dir.
SPIKE_WASM ?= spike.wasm
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 -no-debug -opt=z \
		-wit-package $(SPIKE_WIT) -wit-world spike -o $(SPIKE_WASM) .

# spike.wasm is a committed fixture; deleting it made the polyglot test order-dependent.
clean:
	rm -rf target \
		$(addprefix $(EXT_DIR)/,$(addsuffix .wasm,$(GUESTS))) \
		$(addprefix $(EXT_DIR)/,$(addsuffix .manifest.toml,$(GUESTS)))
