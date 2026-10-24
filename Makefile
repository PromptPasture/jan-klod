.PHONY: help wit all core extensions self-extend-fixture ext ext-new supervisor bundle test test-core test-guests lint-docs harness gate clippy clippy-guests doc audit deny sbom supply-chain npm-supply-chain supervisor-supply-chain lints-drift registry-index registry-index-drift lockfile gate-commit gate-push run serve chat chat-telegram probe config clean install-hooks setup check-spike-deps

.DEFAULT_GOAL := all

include versions.mk

HOST_WS := .
HOST_MK := pkgs/host
EXT := pkgs/extensions
SUPERVISOR := pkgs/host/supervisor

CONFIG := $(abspath config.yaml)
EXT_DIR := $(abspath ext)

# Must match config.yaml's default storage.cache-dir.
CACHE_DIR := $(abspath wasmtime-cache)

# Matches CI's CARGO_TERM_COLOR; the gate's zero-test guard needs it. `?=` allows opt-out.
CARGO_TERM_COLOR ?= always
export CARGO_TERM_COLOR

help:
	@echo "Targets:"
	@echo "  all         build the host workspace + Rust guests (default)"
	@echo "  core        build the host workspace"
	@echo "  extensions  build the Rust guests, staged in ext/ (alias: ext)"
	@echo "  ext-new     scaffold a new extension: NAME=<name> KIND=<kind> [LANG=rust|ts|py]"
	@echo "  bundle      release archive; DIST=coding|headless-chat|minimal for one"
	@echo "  test        run host-side unit tests only (core + guests + supervisor);"
	@echo "              the integration suite needs 'gate' or 'harness' instead"
	@echo "  test-core   run the host workspace's unit tests only (see 'test')"
	@echo "  test-guests run the guests' native tests + the Go supervisor only"
	@echo "  lint-docs   lint every tracked .md with markdownlint-cli2 (needs Node; not in 'test')"
	@echo "  clippy-guests lint the wasm guests, native + wasm32 (-D warnings)"
	@echo "  lints-drift     check the [workspace.lints] blocks of the two cargo workspaces agree"
	@echo "  registry-index  write the registry index for ext/ (REGISTRY_INDEX=path)"
	@echo "  registry-index-drift  check that index generation is deterministic"
	@echo "  harness     build guests, then verify each + the exit-gate flow offline"
	@echo "  gate        build guests, then run the full offline integration exit gate"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  doc         build rustdoc for both workspaces (-D warnings)"
	@echo "  gate-commit the pre-commit gate: fmt + stage guests + core check + doc + test"
	@echo "  gate-push   the pre-push gate: stage guests + test-guests + both clippys + gate"
	@echo "              + registry-index-drift + lockfile + supply-chain, in that order"
	@echo "  supply-chain  run every supply-chain gate (audit + deny + sbom + go + npm)"
	@echo "  audit       cargo-audit the host workspace + every guest (RUSTSEC)"
	@echo "  deny        cargo-deny license/advisory/source policy (host + guests)"
	@echo "  sbom        generate sbom.cdx.json for Rust workspace (cargo-cyclonedx)"
	@echo "  npm-supply-chain  npm lockfile sync + npm audit for pkgs/extensions (TS guest tools)"
	@echo "  supervisor-supply-chain  go mod verify + govulncheck for pkgs/host/supervisor"
	@echo "  run         boot the core against config.yaml + ext/"
	@echo "  probe       drive a live provider completion (needs api key + network)"
	@echo "  config      print the resolved extension plan"
	@echo "  wit         validate the WIT contracts"
	@echo "  setup       install cargo plugins + wkg, fetch wit/spike/deps, configure hooks"
	@echo "  install-hooks  configure git to use .github/hooks/"
	@echo "  clean       remove build artifacts"

# The version check is advisory until the interface freeze.
wit:
	wasm-tools component wit wit/
	sh scripts/wit-version-check.sh

# Checked, not fetched: a fetch would put the network on `gate`.
check-spike-deps:
	@test -d wit/spike/deps || { \
	  echo "error: wit/spike/deps is missing (gitignored, not from git checkout)." >&2; \
	  echo "  Run 'make setup' once per clone, or if you already have wkg:" >&2; \
	  echo "  make -C pkgs/extensions spike-deps" >&2; \
	  exit 1; \
	}

all: core extensions

core:
	$(MAKE) -C $(HOST_MK) build

extensions:
	$(MAKE) -C $(EXT) all

ext: extensions

# make ext-new NAME=tool-hello KIND=tool LANG=ts. LANG ts|py needs jco or componentize-py; only a command-line LANG= counts.
EXT_LANG := $(if $(filter command line,$(origin LANG)),$(LANG),rust)
EXT_NEW_USAGE := usage: make ext-new NAME=<name> KIND=<kind> [LANG=rust|ts|py]
ext-new:
	@test -n "$(NAME)" || { echo "$(EXT_NEW_USAGE)" >&2; exit 2; }
	@test -n "$(KIND)" || { echo "$(EXT_NEW_USAGE)" >&2; exit 2; }
	sh scripts/ext-new.sh "$(NAME)" "$(KIND)" "$(EXT_LANG)"

test: test-core test-guests

test-core:
	$(MAKE) -C $(HOST_MK) test

# Not covered by `gate`: guest native tests and the Go supervisor.
test-guests:
	$(MAKE) -C $(EXT) test
	cd $(SUPERVISOR) && go vet ./... && go test ./...

# Tracked files only: a glob would walk node_modules.
lint-docs:
	git ls-files -z '*.md' | xargs -0 npx --yes markdownlint-cli2@0.23.3

clippy-guests:
	$(MAKE) -C $(EXT) clippy

doc: check-spike-deps
	$(MAKE) -C $(HOST_MK) doc
	$(MAKE) -C $(EXT) doc

# Cargo cannot share [workspace.lints] across workspaces.
lints-drift:
	sh scripts/lints-drift.sh

# index.json is not committed: it carries per-machine digests.
REGISTRY_INDEX ?= $(BUNDLE_OUT)/index.json

# Must carry no query or fragment: `ext install` derives sibling URLs from it.
REGISTRY_URL ?= https://promptpasture.github.io/jan-klod/ext

# A manifest names no author, so the publisher is stated here.
REGISTRY_AUTHOR ?= PromptPasture

registry-index: extensions
	sh scripts/registry-index.sh $(EXT_DIR) $(REGISTRY_URL) $(REGISTRY_AUTHOR) $(REGISTRY_INDEX)

# Called by name from ci.yml so it cannot be skipped.
registry-index-drift: export JK_REQUIRE_GUESTS = 1
registry-index-drift: extensions
	sh scripts/registry-index-drift.sh $(EXT_DIR) $(REGISTRY_URL) $(REGISTRY_AUTHOR)

supervisor:
	cd $(SUPERVISOR) && go build ./...

BUNDLE_OUT ?= $(abspath dist)

# DIST=<name> builds from scripts/distributions/<name>; unset bundles config.yaml and all of ext/.
DIST ?=

bundle: extensions
	cd $(HOST_WS) && cargo build --release -p jk-host -p jk-tui
	@if [ -n "$(DIST)" ]; then 	  sh scripts/dist-stage.sh "$(DIST)" "$(EXT_DIR)" "$(BUNDLE_OUT)/.staged-$(DIST)"; 	  JK_DIST="$(DIST)" sh scripts/bundle.sh $(HOST_WS)/target/release/jan-klod-gateway $(HOST_WS)/target/release/jan-klod 	    "$(BUNDLE_OUT)/.staged-$(DIST)" "$(abspath scripts/distributions/$(DIST)/config.yaml)" $(BUNDLE_OUT) "$(abspath scripts/distributions/$(DIST)/skills)"; 	else 	  sh scripts/bundle.sh $(HOST_WS)/target/release/jan-klod-gateway $(HOST_WS)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT); 	fi

# A green local clippy can still fail in CI: code behind another OS's cfg is never compiled here.
clippy: check-spike-deps
	$(MAKE) -C $(HOST_MK) clippy
	@sh scripts/unlinted-elsewhere.sh $(HOST_MK) pkgs/clients/tui

# --locked: the audited tree must be the tree that builds.
lockfile:
	@for m in $(HOST_WS)/Cargo.toml $(EXT)/Cargo.toml; do \
	  cargo metadata --locked --format-version 1 --manifest-path "$$m" >/dev/null \
	    && echo "lockfile current: $$m" \
	    || { echo "stale lockfile: $$m (run: cargo update --manifest-path $$m)" >&2; exit 1; }; \
	done

audit deny:
	$(MAKE) -C $(HOST_MK) $@
	$(MAKE) -C $(EXT) $@

# Needs cargo-cyclonedx; the results are merged by jaq.
SBOM_DOCS := $(HOST_MK)/*/*.cdx.json pkgs/clients/*/*.cdx.json
sbom:
	cd $(HOST_WS) && cargo cyclonedx --format json --quiet
	@n=$$(ls $(SBOM_DOCS) | wc -l | tr -d ' '); \
	  test "$$n" -eq 7 \
	    || { echo "sbom: expected 7 per-crate documents, found $$n:" >&2; \
	         ls $(SBOM_DOCS) >&2; exit 1; }
	cat $(SBOM_DOCS) | \
	  jaq -s '{bomFormat:.[0].bomFormat,specVersion:.[0].specVersion,version:1,serialNumber:.[0].serialNumber,components:[.[].components//[]|.[]]}' \
	  > sbom.cdx.json

# Run `npm ci --dry-run` first: audit silently passes on a dependency the lock lacks.
# No --omit=dev: every package is a devDependency.
npm-supply-chain:
	cd $(EXT) && npm ci --dry-run && npm audit

# No go.sum: the real check is govulncheck's stdlib scan.
supervisor-supply-chain: export GOFLAGS = -mod=readonly
supervisor-supply-chain:
	cd $(SUPERVISOR) && go mod verify && go run golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION) ./...

supply-chain: deny audit sbom npm-supply-chain supervisor-supply-chain
	$(MAKE) -C $(EXT) go-supply-chain

# Each module name is checked on disk: a typo'd filter could match another module and exit 0.
HARNESS_MODULES := component_harness agent_loop persistence api_rest rpc telegram \
                   host_fs host_process tool_fleet tool_wiring guardrails client_surface contributions_rest
harness: export JK_REQUIRE_GUESTS = 1
harness: check-spike-deps extensions
	@for m in $(HARNESS_MODULES); do \
	  test -f $(HOST_MK)/host/tests/it/$$m.rs \
	    || { echo "harness: no module $$m.rs in $(HOST_MK)/host/tests/it/" >&2; exit 1; }; \
	done
	cd $(HOST_WS) && cargo nextest run -p jk-host --features jk-host/integration $(addsuffix ::,$(HARNESS_MODULES))

# nextest, not `cargo test`: it fails when zero tests match.
# --features jk-host/integration is required, or cargo builds no integration tests and reports success; the guard below catches that.
# --color never: CI's colour escapes defeat the guard's ^ anchor.
# The list runs on its own line so a compile failure is not read as zero.
GATE_LIST := $(HOST_WS)/target/gate-tests.txt

gate: export JK_REQUIRE_GUESTS = 1
gate: check-spike-deps extensions
	@mkdir -p $(dir $(GATE_LIST))
	@cd $(HOST_WS) && cargo nextest list -p jk-host --features jk-host/integration --bins --tests --color never > target/gate-tests.txt
	@n=$$(grep -c '^jk-host::it ' $(GATE_LIST) || true); \
	  echo "gate: integration suite (jk-host::it) resolves $$n test(s)"; \
	  if [ "$$n" -eq 0 ]; then \
	    echo "gate: the integration suite compiled zero tests - the --features integration gate is broken" >&2; \
	    echo "gate: what nextest listed (first 20 lines):" >&2; \
	    head -20 $(GATE_LIST) >&2; \
	    exit 1; \
	  fi
	cd $(HOST_WS) && cargo nextest run --workspace --features jk-host/integration --no-fail-fast
	cd $(HOST_WS) && cargo test --doc --workspace

# The staged-diff skip check stays in the hook: a target cannot see the git index.

# Rebuild ext/: manifests-selftest.sh skips when it is absent.
gate-commit: export JK_REQUIRE_GUESTS = 1
gate-commit:
	cargo fmt --manifest-path $(HOST_WS)/Cargo.toml --all -- --check
	cargo fmt --manifest-path $(EXT)/Cargo.toml --all -- --check
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) -C $(HOST_MK) check
	$(MAKE) doc
	$(MAKE) test

# Both clippys run here, not in `gate`, which stays host-only.
# spike-deps here is the fetch; check-spike-deps only verifies it.
gate-push: export JK_REQUIRE_GUESTS = 1
gate-push:
	$(MAKE) -C $(EXT) spike-deps
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) test-guests
	$(MAKE) clippy
	$(MAKE) clippy-guests
	$(MAKE) gate
	$(MAKE) registry-index-drift
	$(MAKE) lints-drift
	$(MAKE) lockfile
	$(MAKE) deny
	$(MAKE) audit
	$(MAKE) -C $(EXT) go-supply-chain
	$(MAKE) sbom

run:
	cd $(HOST_WS) && cargo run --quiet -p jk-host -- $(CONFIG) $(EXT_DIR)

# Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(HOST_WS) && cargo run --quiet -p jk-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# Override ADDR and SESSION.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(HOST_WS) && cargo run --quiet -p jk-tui -- $(ADDR) $(SESSION)

chat-telegram:
	cd $(HOST_WS) && cargo run --quiet -p jk-host -- telegram $(CONFIG) $(EXT_DIR)

# A real, billed call.
probe:
	cd $(HOST_WS) && cargo run --quiet -p jk-host --features examples --example provider_probe -- $(CONFIG) $(EXT_DIR)

config:
	cd $(HOST_WS) && cargo run --quiet -p jk-config --features examples --example dump -- $(CONFIG)

# Online, run once: the jailed test build is offline and reads this cache. Not part of all, gate or setup.
self-extend-fixture:
	sh scripts/self-extend-fixture.sh

setup:
	cargo install cargo-audit --version $(CARGO_AUDIT_VERSION)
	cargo install cargo-cyclonedx --version $(CARGO_CYCLONEDX_VERSION)
	cargo install --locked cargo-nextest --version $(CARGO_NEXTEST_VERSION)
	cargo install cargo-deny --version $(CARGO_DENY_VERSION)
	cargo install wasm-tools --version $(WASM_TOOLS_VERSION)
	cargo install wkg --version $(WKG_VERSION)
	cargo install jaq --version $(JAQ_VERSION)
	$(MAKE) -C $(EXT) spike-deps
	$(MAKE) install-hooks

install-hooks:
	git config core.hooksPath .github/hooks

clean:
	$(MAKE) -C $(HOST_MK) clean
	$(MAKE) -C $(EXT) clean
	rm -rf $(CACHE_DIR)
