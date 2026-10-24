# Builds the guest components and stages the Rust ones in <repo>/ext. Runs standalone or via the root Makefile.
# Rust guests share one Cargo workspace and Cargo.lock: build them with `-p <guest>` from here.
ROOT := $(abspath $(CURDIR)/../..)
EXT_DIR := $(ROOT)/ext
WIT_DIR := $(ROOT)/wit

# Pinned tool versions. Included via $(ROOT) because CI invokes this makefile directly.
include $(ROOT)/versions.mk

# Rust guests by directory name, staged as ext/<name>.wasm. Adding one here gives it `make <name>`
# and a slot in all/clean. Cargo artifacts swap '-' for '_' (tool-fs -> tool_fs.wasm).
GUESTS := host-secrets-probe interceptor-context interceptor-guardrails interceptor-intent-router interceptor-permission interceptor-persona interceptor-principal-probe interceptor-system interceptor-task-router interceptor-tool-selector provider-anthropic provider-openai registry-mcp registry-skills tool-ask-probe tool-edit tool-escape-probe tool-fetch tool-find tool-fs tool-git tool-hello tool-memory tool-plan tool-proc tool-proc-probe tool-shell tool-web-search

# Repo-root deny.toml, shared with the host workspace.
DENY_CONFIG := $(ROOT)/deny.toml

# Go modules: any */go.mod, so a new Go guest is gated without editing this file.
GO_MODULES := $(wildcard */go.mod)

# Pure-Rust libraries: tested, never built to wasm or staged.
LIBS := guest-fs

# Guests with host-target unit tests. Their wasm32 glue is cfg-gated, so a native `cargo test`
# covers only the pure logic. Add a guest here once it has such tests.
TESTABLE_GUESTS := interceptor-context interceptor-guardrails interceptor-intent-router interceptor-permission interceptor-system interceptor-task-router interceptor-tool-selector test-interceptor-persona tool-edit tool-fetch tool-find tool-fs tool-git tool-memory tool-plan tool-shell tool-web-search

.PHONY: all stage manifests clean test clippy audit deny go-supply-chain spike-deps spike-guest ts-guest py-guest $(GUESTS)

# One cargo invocation for all Rust guests, so cargo parallelises them. The TinyGo spike stays opt-in.
# ts-guest is not opt-in: it skips without jco, so opting in would leave it untested (#130).
all: manifests ts-guest py-guest

# Staged components, then manifests, which depend on this recipe's output.
stage:
	cargo build --release --target wasm32-wasip2 $(addprefix -p ,$(GUESTS))
	mkdir -p $(EXT_DIR)
	@for g in $(GUESTS); do \
		cp target/wasm32-wasip2/release/$$(echo $$g | tr - _).wasm $(EXT_DIR)/$$g.wasm; \
	done

# Generated, not committed: ext/ is gitignored build output, and a committed manifest would describe
# a component the repo does not contain. Built beside the component; `clean` removes both.
manifests: stage
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $(GUESTS)
	sh $(ROOT)/scripts/manifests-verify.sh $(EXT_DIR) $(GUESTS)

# Native unit tests (host target, no wasm). Only LIBS and TESTABLE_GUESTS are iterated.
test:
	@for g in $(LIBS) $(TESTABLE_GUESTS); do echo "== test $$g =="; cargo test -p $$g || exit 1; done
	@echo "== manifest guards =="
	sh $(ROOT)/scripts/manifests-selftest.sh $(EXT_DIR)
	@echo "== ext-new template =="
	sh $(ROOT)/scripts/ext-new-selftest.sh

# TypeScript guest (#187). Skips with a message when jco or esbuild is missing, never silently.
# esbuild is pinned in package.json. jco is expected on PATH: vendoring it would add 326 MB of node_modules.
# `--disable http --disable fetch-event` is required: otherwise the component imports wasi:http,
# which the host refuses. Guests reach the network only through host-http, where the egress policy is.
# TS_GUESTS is written by scripts/ext-new.sh, like GUESTS; the world comes from the name prefix.
TS_GUESTS := tool-hello-ts
ESBUILD := $(CURDIR)/node_modules/.bin/esbuild

.PHONY: ts-guest
# One shell block, not separate recipe lines: a guard's exit 0 would only end its own line,
# and make would run the build anyway.
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

# Python guest (#188), shaped like ts-guest. componentize-py generates its own bindings, so there is
# no bundler step. Expected on PATH, not vendored, like jco.
# Differs from the TypeScript path in two ways:
#   * `bindings` runs before `componentize`, and the generated wit_world is removed first:
#     `bindings` refuses to overwrite it and fails with 'File exists' on the second build.
#   * No `--disable` flags exist: CPython initialises wasi:filesystem, wasi:sockets and wasi:cli.
#     They are inert, and sandbox_boundary.rs tests each refusal (see docs/concepts/security-model.md).
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

# Lint twice: the native run sees the pure logic, and only the wasm run sees the cfg-gated
# Component glue. [workspace.lints] only means something if both run (#205).
clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --target wasm32-wasip2 -- -D warnings

# Build and stage one guest for dev iteration, e.g. `make tool-fs`. Needs `rustup target add wasm32-wasip2`.
$(GUESTS):
	cargo build --release --target wasm32-wasip2 -p $@
	mkdir -p $(EXT_DIR)
	cp target/wasm32-wasip2/release/$(subst -,_,$@).wasm $(EXT_DIR)/$@.wasm
	# Regenerate the manifest too: a stale one beside a rebuilt component would go unnoticed.
	sh $(ROOT)/scripts/manifests.sh $(CURDIR) $(EXT_DIR) $(ROOT)/wit $@


# Supply-chain gates for the Rust guests. One Cargo.lock covers the workspace, so one invocation suffices.
audit:
	cargo audit

deny:
	cargo deny -L error --manifest-path Cargo.toml --config $(DENY_CONFIG) check

# Go modules: verified sums plus a govulncheck scan. -mod=readonly stops the scan rewriting go.mod or go.sum.
# The scanner version is pinned in versions.mk (#131).
go-supply-chain: export GOFLAGS = -mod=readonly
go-supply-chain:
	@for mod in $(GO_MODULES); do dir=$$(dirname $$mod); echo "== go supply-chain $$dir =="; \
		( cd $$dir && go mod verify && go run golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION) ./... ) || exit 1; done

# Resolve the spike world's WIT deps (wasi:cli and friends) into wit/spike/deps.
spike-deps:
	cd $(WIT_DIR) && wkg wit fetch --wit-dir spike

# TinyGo spike: the Slice 1a gate canary, not a real guest. Kept out of GUESTS and `all`.
# spike.wasm is a committed fixture (see .gitignore), so the polyglot test needs no TinyGo. Rebuild with
# `make spike-guest` and commit. -no-debug -opt=z cuts it from 562 KB to 75 KB, at the cost of panic DWARF.
SPIKE_DIR := spike
SPIKE_WIT := $(WIT_DIR)/spike
# Overridable so the polyglot drift test can rebuild into a temp dir, not over the committed fixture.
# Relative paths resolve inside $(SPIKE_DIR).
SPIKE_WASM ?= spike.wasm
spike-guest: spike-deps
	cd $(SPIKE_DIR) && tinygo build -target=wasip2 -no-debug -opt=z \
		-wit-package $(SPIKE_WIT) -wit-world spike -o $(SPIKE_WASM) .

# spike.wasm is not removed: it is a committed fixture, and deleting it made the polyglot test depend on run order.
clean:
	rm -rf target \
		$(addprefix $(EXT_DIR)/,$(addsuffix .wasm,$(GUESTS))) \
		$(addprefix $(EXT_DIR)/,$(addsuffix .manifest.toml,$(GUESTS)))
