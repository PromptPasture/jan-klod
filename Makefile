.PHONY: help wit all core extensions ext ext-new supervisor bundle test test-core test-guests harness gate clippy audit deny sbom supply-chain lockfile gate-commit gate-push run serve chat chat-telegram probe config clean install-hooks setup check-spike-deps

.DEFAULT_GOAL := all

# Orchestrates two sub-makefiles: src/core/Makefile (host workspace) and
# src/extensions/Makefile (guest components, staged in ext/). Also owns the
# root WIT contracts and the integration targets spanning both subtrees.

CORE := src/core
EXT := src/extensions
SUPERVISOR := src/supervisor

# EXT_DIR mirrors the extensions sub-makefile's staging dir.
CONFIG := $(abspath config.yaml)
EXT_DIR := $(abspath ext)

# The shipped config.yaml's storage.cache-dir default (#60): Wasmtime's
# compiled-component cache, beside config.yaml unless an operator overrides
# the key. A path this Makefile assumes rather than reads out of the YAML —
# the same convention CONFIG/EXT_DIR already are.
CACHE_DIR := $(abspath wasmtime-cache)

# Colour, set here so a local gate runs in CI's environment rather than its own.
#
# `.github/workflows/ci.yml` sets `CARGO_TERM_COLOR: always`, and cargo and
# nextest honour it even when stdout is a pipe. Locally nothing set it, so the
# default `auto` gave *uncoloured* output into exactly the pipes these Makefiles
# parse — and `gate`'s zero-test guard, which counts lines of `nextest list`,
# matched 172 locally and 0 in CI (#123). `.github/hooks/pre-push` therefore
# passed on every push that CI then failed.
#
# The hook already says the gate sequence lives here "so this hook cannot drift
# from CI". The environment it runs in has to live here for the same reason,
# or the sequence matching proves nothing. `?=` so a developer who wants plain
# output can still ask: `CARGO_TERM_COLOR=never make gate`.
CARGO_TERM_COLOR ?= always
export CARGO_TERM_COLOR

# List the common targets.
help:
	@echo "Targets:"
	@echo "  all         build the host workspace + Rust guests (default)"
	@echo "  core        build the host workspace"
	@echo "  extensions  build the Rust guests, staged in ext/ (alias: ext)"
	@echo "  ext-new     scaffold a new extension crate: NAME=<name> KIND=<kind>"
	@echo "  test        run host-side unit tests only (core + guests + supervisor);"
	@echo "              the integration suite needs 'gate' or 'harness' instead"
	@echo "  test-core   run the host workspace's unit tests only (see 'test')"
	@echo "  test-guests run the guests' native tests + the Go supervisor only"
	@echo "  harness     build guests, then verify each + the exit-gate flow offline"
	@echo "  gate        build guests, then run the full offline integration exit gate"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  gate-commit the pre-commit gate: fmt + stage guests + core check + test"
	@echo "  gate-push   the pre-push gate: stage guests + test-guests + clippy + gate"
	@echo "              + lockfile + supply-chain, in that order"
	@echo "  supply-chain  run every supply-chain gate (audit + deny + sbom + go)"
	@echo "  audit       cargo-audit the host workspace + every guest (RUSTSEC)"
	@echo "  deny        cargo-deny license/advisory/source policy (host + guests)"
	@echo "  sbom        generate sbom.cdx.json for Rust workspace (cargo-cyclonedx)"
	@echo "  run         boot the core against config.yaml + ext/"
	@echo "  probe       drive a live provider completion (needs api key + network)"
	@echo "  config      print the resolved extension plan"
	@echo "  wit         validate the WIT contracts"
	@echo "  setup       install cargo plugins + wkg, fetch wit/spike/deps, configure hooks"
	@echo "  install-hooks  configure git to use .github/hooks/"
	@echo "  clean       remove build artifacts"

# Root-level WIT contracts are language-neutral, so they stay a root concern.
#
# Two checks, and the order matters: validate the contracts first, then ask
# whether an edit to them bumped the package version. A warning about the
# version of a file that does not parse would be noise on top of a real error.
# The version check is advisory (always exits 0) until the interface freeze.
wit:
	wasm-tools component wit wit/
	sh scripts/wit-version-check.sh

# wit/spike/deps (wasi:cli & co) is gitignored and only ever populated by `make
# setup` (below) or `make -C src/extensions spike-deps` — nothing on the plain
# build path fetches it. Without this check, a fresh clone that skipped setup
# hits `bindgen!`'s compile-time WIT resolution instead: "failed to resolve
# directory while parsing WIT for path .../wit/spike", which names no fix.
# Checked, not fetched — an actual `wkg wit fetch` here would put a network
# call on `gate`, which its own comment calls the offline suite.
check-spike-deps:
	@test -d wit/spike/deps || { \
	  echo "error: wit/spike/deps is missing (gitignored, not from git checkout)." >&2; \
	  echo "  Run 'make setup' once per clone, or if you already have wkg:" >&2; \
	  echo "  make -C src/extensions spike-deps" >&2; \
	  exit 1; \
	}

all: core extensions

# --- Build (delegated to the sub-makefiles) ---
core:
	$(MAKE) -C $(CORE) build

extensions:
	$(MAKE) -C $(EXT) all

# Alias: `ext` is the name every skip notice tells developers to run, but
# `ext/` is also a directory, so plain make no-ops on it without .PHONY.
ext: extensions

# Scaffold a new extension crate, registered and ready to build.
#
#   make ext-new NAME=tool-hello KIND=tool
#
# KIND ∈ provider | tool | interceptor | registry-skills | registry-mcp. Not
# `agent`: there is no agent world in wit/ and `Runtime::boot` has no arm for
# one, so a generated agent crate could compile against nothing and never load
# (#111).
ext-new:
	@test -n "$(NAME)" || { echo "usage: make ext-new NAME=<name> KIND=<kind>" >&2; exit 2; }
	@test -n "$(KIND)" || { echo "usage: make ext-new NAME=<name> KIND=<kind>" >&2; exit 2; }
	sh scripts/ext-new.sh "$(NAME)" "$(KIND)"


# Host-side unit tests: the core workspace plus the guests' native tests.
test: test-core test-guests

test-core:
	$(MAKE) -C $(CORE) test

# The legs `gate` doesn't cover: guest native tests + the Go supervisor.
test-guests:
	$(MAKE) -C $(EXT) test
	cd $(SUPERVISOR) && go vet ./... && go test ./...

# Tiny Go blue/green supervisor (static, dependency-free binary).
supervisor:
	cd $(SUPERVISOR) && go build ./...

# Self-contained release bundle: core binary + staged guests + config + README,
# as dist/jan-klod-<version>-<os>-<arch>.tar.gz.
BUNDLE_OUT ?= $(abspath dist)
bundle: extensions
	cd $(CORE) && cargo build --release -p jan-klod-host -p jan-klod
	sh scripts/bundle.sh $(CORE)/target/release/jan-klod-gateway $(CORE)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT)

clippy: check-spike-deps
	$(MAKE) -C $(CORE) clippy

# --- Supply-chain gates (CI enforces all of these) ---

# Verify each Cargo.lock is current against its manifest — `--locked` fails
# rather than silently re-resolving, so a stale lock cannot slip an
# audited/denied tree that differs from the one that actually builds. Two
# workspaces: the host workspace and the guest extensions workspace (one
# shared Cargo.lock across all guests, so a new guest added to
# src/extensions/Cargo.toml's members is covered without editing this list).
# This used to be a loop inlined in both `pre-push` and `ci.yml`'s
# supply-chain job — the same commands typed twice, free to drift the way
# the rest of #72 was about. Named here, both now call it.
lockfile:
	@for m in $(CORE)/Cargo.toml $(EXT)/Cargo.toml; do \
	  cargo metadata --locked --format-version 1 --manifest-path "$$m" >/dev/null \
	    && echo "lockfile current: $$m" \
	    || { echo "stale lockfile: $$m (run: cargo update --manifest-path $$m)" >&2; exit 1; }; \
	done

# Each subtree owns its own audit/deny invocation; root fans out to both.
audit deny:
	$(MAKE) -C $(CORE) $@
	$(MAKE) -C $(EXT) $@

# CycloneDX SBOM for all Rust crates. Install once: cargo install cargo-cyclonedx
sbom:
	cd src/core && cargo cyclonedx --format json --quiet
	jq -s '{bomFormat:.[0].bomFormat,specVersion:.[0].specVersion,version:1,serialNumber:.[0].serialNumber,components:[.[].components//[]|.[]]}' \
	  src/core/**/*.cdx.json > sbom.cdx.json

# License/advisory/source policy + RUSTSEC audit (host + guests), Go vuln
# scan (guests + supervisor), and the SBOM.
supply-chain: deny audit sbom
	$(MAKE) -C $(EXT) go-supply-chain
	cd $(SUPERVISOR) && GOFLAGS=-mod=readonly go mod verify && govulncheck ./...

# --- Integration (host + staged extensions; spans both subtrees) ---

# Build guests, then verify each through the Component Model, offline (canned
# host-http, no api key, no network):
#   component_harness — each guest's WIT interface + lifecycle, in isolation
#   agent_loop         — the config-driven agent loop (greeting + multi-step)
#   persistence        — a transcript survives a Runtime restart (SQLite store)
#   api_rest           — a turn driven over the host-side REST surface
#   telegram           — a canned inbound message drives a turn + reply
#   host_fs            — guest read/write through host-fs; escapes denied
#   host_process       — guest command through host-process; denied when disabled
#   tool_fleet         — ToolFleet dispatches a call to the matching tool-* guest
#   tool_wiring        — build_agent wires an enabled tool.* into the fleet
# These are modules of one test binary (src/core/host/tests/it/main.rs), run by
# --test name filter. A typo'd filter can still match other modules and exit 0,
# so each name is checked against the filesystem before the run.
HARNESS_MODULES := component_harness agent_loop persistence api_rest rpc telegram \
                   host_fs host_process tool_fleet tool_wiring
harness: export JK_REQUIRE_GUESTS = 1
harness: check-spike-deps extensions
	@for m in $(HARNESS_MODULES); do \
	  test -f $(CORE)/host/tests/it/$$m.rs \
	    || { echo "harness: no module $$m.rs in $(CORE)/host/tests/it/" >&2; exit 1; }; \
	done
	cd $(CORE) && cargo nextest run -p jan-klod-host --features jan-klod-host/integration $(addsuffix ::,$(HARNESS_MODULES))

# Exit gate: the full offline integration suite, nothing allowed to skip.
# JK_REQUIRE_GUESTS turns a silently-skipped test into a failure.
#
# #79 verified this gate's test count is unchanged by the feature gate below:
# 504 tests run, 0 skipped (160 of them jan-klod-host::it, the rest unit
# tests across the workspace) under JK_REQUIRE_GUESTS=1, same as immediately
# before the `integration` feature landed. That absolute count will drift as
# the suite grows — it is not itself a gate — but a *drop to zero* is exactly
# what the guard below exists to catch.
#
# nextest over `cargo test`: (1) one process per test — some modules set
# conflicting process-global env vars, which race under in-binary threading;
# (2) nextest exits nonzero on zero tests matched, `cargo test` exits 0.
# --no-fail-fast reports every failure in the run, not just the first.
# Doctests run separately since nextest doesn't run them.
#
# --features jan-klod-host/integration cannot be dropped: since #79 the `it`
# target (tests/it/, the whole integration suite) carries `required-features =
# ["integration"]`, so without this flag cargo simply does not build it — no
# wasmtime link, no test, and critically no *failure* either. `cargo nextest
# run --workspace` without the feature still finds every other crate's unit
# tests, runs them, and exits 0, having never touched integration coverage. In
# the same spirit as the note above about not naming individual test files:
# dropping this flag is a second, quieter way for the whole suite to vanish
# that JK_REQUIRE_GUESTS cannot catch, since the tests are never compiled to
# check anything. The guard below is what actually catches it.
#
# Positive guard, not a convention: fail the gate if the integration suite
# resolved to zero tests, rather than trusting that the feature flag above is
# never dropped or mistyped. `cargo nextest list` is cheap (no run, just
# compiles-and-enumerates) and prints one line per test as
# "<binary> <test-name>"; the `it` binary's lines start with
# "jan-klod-host::it ".
#
# `--color never` is what makes that last sentence true, and it is not
# cosmetic. `.github/workflows/ci.yml` sets `CARGO_TERM_COLOR: always`, which
# nextest honours even when stdout is a pipe: every line then *begins* with an
# SGR escape, the `^` anchor below matches nothing, and the guard reports zero
# tests for a suite that compiled all of them. It did exactly that, and held
# `main` red for seven runs (#123) — measured on one tree: 172 matches
# uncoloured, 0 coloured, 172 with this flag. Nothing local sets the variable,
# so `.github/hooks/pre-push` ran the same guard green every time.
#
# The list also gets its own line now. Folded into the `$(…)` with
# `2>/dev/null`, a compile failure and a zero count were one outcome — "exit 1"
# with no output — and the redirect discarded the error that said which. Let
# cargo fail on its own exit code, then count from the file.
GATE_LIST := $(CORE)/target/gate-tests.txt

gate: export JK_REQUIRE_GUESTS = 1
gate: check-spike-deps extensions
	@mkdir -p $(dir $(GATE_LIST))
	@cd $(CORE) && cargo nextest list -p jan-klod-host --features jan-klod-host/integration --bins --tests --color never > target/gate-tests.txt
	@n=$$(grep -c '^jan-klod-host::it ' $(GATE_LIST) || true); \
	  echo "gate: integration suite (jan-klod-host::it) resolves $$n test(s)"; \
	  if [ "$$n" -eq 0 ]; then \
	    echo "gate: the integration suite compiled zero tests - the --features integration gate is broken" >&2; \
	    echo "gate: what nextest listed (first 20 lines):" >&2; \
	    head -20 $(GATE_LIST) >&2; \
	    exit 1; \
	  fi
	cd $(CORE) && cargo nextest run --workspace --features jan-klod-host/integration --no-fail-fast
	cd $(CORE) && cargo test --doc --workspace

# --- Hook-facing gates (also called directly by .github/workflows/ci.yml) ---
#
# `.github/hooks/pre-commit` and `.github/hooks/pre-push` used to carry these
# two sequences as bash: each hook a hand-ordered list of `make` calls that
# only the hook itself could run and that `ci.yml` had no way to reuse — two
# independent copies of "what a commit/push must pass," free to drift the way
# #65 did (`pre-push` ran the host workspace suite twice, and nothing but
# reading the bash would have caught it). Naming them here means a developer
# can run either by hand, and the hook and `ci.yml` share one place these
# steps are listed instead of restating them.
#
# The staged-diff skip check that lets a doc-only commit skip `pre-commit`
# instantly stays in the hook itself, not here: it reads the git index
# (`git diff --cached --name-only`), and a target has no notion of a staged
# diff to read.

# The pre-commit gate: both `cargo fmt --check` invocations, then a rebuilt
# (not just removed) ext/ — an absent ext/ makes
# scripts/manifests-selftest.sh (run by `make -C src/extensions test`, part of
# `make test` below) skip instead of checking anything, so `rm -rf` alone
# would let a missing manifest report green. JK_REQUIRE_GUESTS=1 is what turns
# a skipped self-test into a failure rather than trusting the rebuild alone to
# keep it honest, the same reasoning `gate`'s own export above uses.
gate-commit: export JK_REQUIRE_GUESTS = 1
gate-commit:
	cargo fmt --manifest-path $(CORE)/Cargo.toml --all -- --check
	cargo fmt --manifest-path $(EXT)/Cargo.toml --all -- --check
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) -C $(CORE) check
	$(MAKE) test

# The pre-push gate: a push pays the full gate unconditionally, no skip check.
# `spike-deps` here is the *fetch*, not the `check-spike-deps` prerequisite
# `clippy` and `gate` already carry below — that target only verifies
# wit/spike/deps exists, so a clone that skipped `make setup` still needs this
# explicit fetch before clippy/gate can compile against it. Guests are
# rebuilt for the same reason `gate-commit` rebuilds them. `test-guests`, not
# `test`, runs here: `gate` below is a strict superset of the host workspace's
# unit tests once --features jan-klod-host/integration is added, so running
# `test-core` a second time here would just repeat it for no extra coverage.
gate-push: export JK_REQUIRE_GUESTS = 1
gate-push:
	$(MAKE) -C $(EXT) spike-deps
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) test-guests
	$(MAKE) clippy
	$(MAKE) gate
	$(MAKE) lockfile
	$(MAKE) deny
	$(MAKE) audit
	$(MAKE) -C $(EXT) go-supply-chain
	$(MAKE) sbom

# Boot the real core against config.yaml: resolve extensions against ext/,
# compile present components, run their lifecycle, print the boot plan.
run:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Serve the loop over the host-side REST surface. Uses live host-http, so the
# enabled provider needs its api-key env. Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# TUI client — connects to the gateway (auto-starting it if not running).
# Override ADDR=host:port and SESSION=id.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(CORE) && cargo run --quiet -p jan-klod -- $(ADDR) $(SESSION)

# Telegram bot (headless). Needs TELEGRAM_BOT_TOKEN + network.
chat-telegram:
	cd $(CORE) && cargo run --quiet -p jan-klod-host -- telegram $(CONFIG) $(EXT_DIR)

# Drives a provider's llm-provider.complete path against a live endpoint.
# Needs the provider's api-key env (e.g. OPENAI_API_KEY) — real, billed call.
probe:
	cd $(CORE) && cargo run --quiet -p jan-klod-host --features examples --example provider_probe -- $(CONFIG) $(EXT_DIR)

# Resolve config.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(CORE) && cargo run --quiet -p jan-klod-config --features examples --example dump -- $(CONFIG)

# Every tool whose output or CLI these targets depend on is pinned, and each pin
# has a twin in .github/workflows/ci.yml. Unpinned, CI resolves `@latest` while a
# developer machine keeps whatever it installed months ago, and the two disagree
# without either being wrong.
#
# cargo-deny is why: 0.20 removed `--config` from the `check` subcommand and made
# it global, so 0.19 and 0.20 need different invocations and no spelling
# satisfies both. CI had `@latest` (0.20.2), local machines had 0.19, and the
# supply-chain job failed on main for days. The other three carry the identical
# exposure — cargo-nextest most of all, since these Makefiles read its output —
# so they are pinned to the versions current when this was written, which are
# also the ones this repository has been verified against.
CARGO_DENY_VERSION := 0.20.2
CARGO_AUDIT_VERSION := 0.22.2
CARGO_CYCLONEDX_VERSION := 0.5.9
CARGO_NEXTEST_VERSION := 0.9.143
# Not a cargo plugin, but the same rule applies: `make extensions` reads its
# output to generate each guest's capability manifest, so a change to how it
# prints a component's WIT lands on us.
WASM_TOOLS_VERSION := 1.258.0
# wkg resolves wit/spike/deps against the committed wit/wkg.lock (#77). Pinned
# for the same reason as everything else here, and to the same value CI installs
# it at (see the "Install wkg" steps in .github/workflows/ci.yml): a lockfile is
# a resolution of *some* registry state at the time it was written, and a newer
# wkg is not guaranteed to reproduce it.
WKG_VERSION := 0.15.1

# One-time developer setup: cargo supply-chain plugins, git hooks, and the WIT
# deps that only `wkg` can fetch.
setup:
	cargo install cargo-audit --version $(CARGO_AUDIT_VERSION)
	cargo install cargo-cyclonedx --version $(CARGO_CYCLONEDX_VERSION)
	cargo install cargo-nextest --version $(CARGO_NEXTEST_VERSION)
	cargo install cargo-deny --version $(CARGO_DENY_VERSION)
	cargo install wasm-tools --version $(WASM_TOOLS_VERSION)
	cargo install wkg --version $(WKG_VERSION)
	$(MAKE) -C $(EXT) spike-deps
	$(MAKE) install-hooks

install-hooks:
	git config core.hooksPath .github/hooks

clean:
	$(MAKE) -C $(CORE) clean
	$(MAKE) -C $(EXT) clean
	rm -rf $(CACHE_DIR)
