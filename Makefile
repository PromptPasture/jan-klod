.PHONY: help wit all core extensions ext ext-new supervisor gui bundle test test-core test-guests test-web test-gui harness gate clippy clippy-gui clippy-guests audit deny sbom supply-chain web-supply-chain supervisor-supply-chain web-dist-drift registry-index registry-index-drift lockfile gate-commit gate-push run serve chat chat-gui chat-telegram probe config clean install-hooks setup check-spike-deps

.DEFAULT_GOAL := all

# Orchestrates two sub-makefiles: src/Makefile (the Rust host workspace) and
# src/extensions/Makefile (guest components, staged in ext/). Also owns the
# root WIT contracts and the integration targets spanning both subtrees.

# Pinned tool versions, shared with src/extensions/Makefile, which includes the
# same file. See versions.mk for why it is a file and not a block here.
include versions.mk

HOST_WS := src
EXT := src/extensions
SUPERVISOR := src/supervisor
WEB := src/web
# The Tauri shell (#141, #142). Its own cargo workspace, deliberately: Tauri
# resolves 329 packages nothing else here needs, and as a member of $(HOST_WS) they
# would be on every `cargo test` and every CI run. Nothing on the default build
# path reaches it — `all` does not, `gate` does not — which is the point. Its
# supply-chain gates are *not* optional in the same way: `lockfile`, `deny` and
# `audit` below all name it.
GUI_DIR := src/gui

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
	@echo "  ext-new     scaffold a new extension: NAME=<name> KIND=<kind> [LANG=rust|ts|py]"
	@echo "  gui         build the Tauri shell and stage it beside jan-klod"
	@echo "  bundle      release archive; DIST=coding|headless-chat|minimal for one,"
	@echo "              GUI=1 to ship the window too (archive gains '-gui')"
	@echo "  test        run host-side unit tests only (core + guests + supervisor);"
	@echo "              the integration suite needs 'gate' or 'harness' instead"
	@echo "  test-core   run the host workspace's unit tests only (see 'test')"
	@echo "  test-guests run the guests' native tests + the Go supervisor only"
	@echo "  test-web    run the browser client's suite (src/web; needs Node, not in 'test')"
	@echo "  test-gui    run the Tauri shell's suite (src/gui; needs a display, not in 'test')"
	@echo "  clippy-gui  lint the Tauri shell (-D warnings)"
	@echo "  clippy-guests lint the wasm guests, native + wasm32 (-D warnings)"
	@echo "  web-dist-drift  check src/web/dist/ is still what src/web/src/ builds"
	@echo "  registry-index  write the registry index for ext/ (REGISTRY_INDEX=path)"
	@echo "  registry-index-drift  check that index generation is deterministic"
	@echo "  harness     build guests, then verify each + the exit-gate flow offline"
	@echo "  gate        build guests, then run the full offline integration exit gate"
	@echo "  clippy      lint the host workspace (-D warnings)"
	@echo "  gate-commit the pre-commit gate: fmt + stage guests + core check + test"
	@echo "  gate-push   the pre-push gate: stage guests + test-guests + all three clippys + gate"
	@echo "              + registry-index-drift + lockfile + supply-chain, in that order"
	@echo "  supply-chain  run every supply-chain gate (audit + deny + sbom + go + web)"
	@echo "  audit       cargo-audit the host workspace + every guest (RUSTSEC)"
	@echo "  deny        cargo-deny license/advisory/source policy (host + guests)"
	@echo "  sbom        generate sbom.cdx.json for Rust workspace (cargo-cyclonedx)"
	@echo "  web-supply-chain  npm lockfile sync + npm audit for src/web"
	@echo "  supervisor-supply-chain  go mod verify + govulncheck for src/supervisor"
	@echo "  run         boot the core against config.yaml + ext/"
	@echo "  chat-gui    open the web client in a window (builds the shell first)"
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
	$(MAKE) -C $(HOST_WS) build

extensions:
	$(MAKE) -C $(EXT) all

# Alias: `ext` is the name every skip notice tells developers to run, but
# `ext/` is also a directory, so plain make no-ops on it without .PHONY.
ext: extensions

# Scaffold a new extension, registered and ready to build.
#
#   make ext-new NAME=tool-hello KIND=tool
#   make ext-new NAME=tool-hello KIND=tool LANG=ts
#
# KIND ∈ provider | tool | interceptor | registry-skills | registry-mcp. Not
# `agent`: there is no agent world in wit/ and `Runtime::boot` has no arm for
# one, so a generated agent crate could compile against nothing and never load
# (#111).
#
# LANG ∈ rust (default) | ts | py. Both non-Rust paths cover tool and
# interceptor only, and their builds need a toolchain `make setup` does not
# install — `jco` (326 MB) or `componentize-py` (50 MB), for the reasons in
# versions.mk (#187, #188).
#
# `LANG` is also the locale variable, and make imports the environment, so a
# bare `$(LANG)` reads `en_US.UTF-8` on most machines. Only a command-line
# `LANG=` is the language axis; the inherited locale is left alone, both here
# and in every recipe that runs a locale-sensitive tool.
EXT_LANG := $(if $(filter command line,$(origin LANG)),$(LANG),rust)
EXT_NEW_USAGE := usage: make ext-new NAME=<name> KIND=<kind> [LANG=rust|ts|py]
ext-new:
	@test -n "$(NAME)" || { echo "$(EXT_NEW_USAGE)" >&2; exit 2; }
	@test -n "$(KIND)" || { echo "$(EXT_NEW_USAGE)" >&2; exit 2; }
	sh scripts/ext-new.sh "$(NAME)" "$(KIND)" "$(EXT_LANG)"


# Host-side unit tests: the core workspace plus the guests' native tests.
test: test-core test-guests

test-core:
	$(MAKE) -C $(HOST_WS) test

# The legs `gate` doesn't cover: guest native tests + the Go supervisor.
test-guests:
	$(MAKE) -C $(EXT) test
	cd $(SUPERVISOR) && go vet ./... && go test ./...

# The browser client's suite (#127). Deliberately **not** a prerequisite of
# `test` above, which is what keeps it off the pre-commit path `gate-commit`
# runs: the npm legs are CI-only here, the way `web-supply-chain` already is,
# so a contributor without Node can still run every local gate. That is the
# same cost #119 box 1 declined when it kept Node off the Rust build path.
#
# `ci.yml` calls this by name, and that is what stops it becoming the target
# #130 describes — aggregated somewhere nothing invokes, which looks exactly
# like a gate that passes.
#
# No `npm ci` first, and that is a property of the suite rather than an
# omission: tests/turn.test.ts imports `node:test`, `node:assert` and a local
# ./dom.ts, and nothing else, so it needs Node on PATH and no install at all.
# package.json's `test` script runs the .ts sources directly via
# --experimental-strip-types, which needs Node >= 22.6.
test-web:
	cd $(WEB) && npm test

# The Tauri shell's suite (#142). Deliberately **not** a prerequisite of `test`,
# for the same reason `test-web` is not: it belongs to a workspace that is not
# on the default build path, and folding it in would put a 256-package build and
# a ~40s link on every contributor's pre-commit.
#
# `ci.yml` calls this by name — the rule from #130, and it bites hardest here,
# because this is the only suite in the repository that can legitimately skip
# itself. The skip is what JK_REQUIRE_GUI turns back into a failure, and CI sets
# it on the jobs that actually have a display.
test-gui:
	$(MAKE) -C $(GUI_DIR) test

clippy-gui:
	$(MAKE) -C $(GUI_DIR) clippy

# The guests are their own cargo workspace too, and declare the same strict
# lint policy — see `src/extensions/Makefile` for why this runs twice.
clippy-guests:
	$(MAKE) -C $(EXT) clippy

# The committed bundle is still what the sources build (#127). Kept out of
# `test-web` on cost: that leg needs Node on PATH and installs nothing, while
# this one runs `npm ci` against the pinned toolchain, so folding them together
# would make the cheap check pay for the expensive one. CI-only for the same
# reason `test-web` is — see the comment above it. The script carries why the
# rebuild happens in a temp tree and why `npm ci` is not `npm install`.
web-dist-drift:
	sh scripts/web-dist-drift.sh

# --- Registry index (Slice 16d-1) ---
#
# `index.json`: what a registry can say about each staged component *before*
# anyone downloads it — above all the capabilities it asks the host for, which
# is what the manifest exists to make knowable in advance.
#
# Not committed, unlike this repository's other generated artifacts. It
# describes `ext/`, and `ext/*.wasm` is gitignored and rebuilt per clone, so a
# committed index would carry digests that differ per machine. That is also why
# `registry-index-drift` checks reproducibility rather than diffing against a
# committed file — the script says the rest.
#
# Output beside the release archives, because that is what the index describes:
# the individual `.wasm` and `.manifest.toml` files a release publishes at plain
# paths, which `ext install` fetches one by one and verifies per file.
REGISTRY_INDEX ?= $(BUNDLE_OUT)/index.json

# Where the files the index names will actually be served from. The default is
# this repository's GitHub Pages path; publishing the first-party index is
# 16d-3's slice, and it overrides this with wherever that release puts the
# files. A base carrying a query or fragment is refused by the generator — `ext
# install` derives the manifest and signature URLs as siblings, and a relative
# reference drops a query.
REGISTRY_URL ?= https://promptpasture.github.io/jan-klod/ext

# Who published this index. A manifest names no author — it is generated from a
# component's own imports — so this is the one field that is stated rather than
# derived, and stating it once here beats inventing a per-component answer.
REGISTRY_AUTHOR ?= PromptPasture

registry-index: extensions
	sh scripts/registry-index.sh $(EXT_DIR) $(REGISTRY_URL) $(REGISTRY_AUTHOR) $(REGISTRY_INDEX)

# Called by name from `ci.yml`, which is what makes it a gate rather than a
# target somebody could type (#130). JK_REQUIRE_GUESTS turns "no manifests
# staged, nothing to check" from a skip into a failure, the same way `gate` and
# `gate-commit` do.
registry-index-drift: export JK_REQUIRE_GUESTS = 1
registry-index-drift: extensions
	sh scripts/registry-index-drift.sh $(EXT_DIR) $(REGISTRY_URL) $(REGISTRY_AUTHOR)

# Tiny Go blue/green supervisor (static, dependency-free binary).
supervisor:
	cd $(SUPERVISOR) && go build ./...

# The Tauri shell, built and put where `jan-klod --gui` will look for it.
#
# The staging step is the whole reason this is a target rather than a `cd`:
# `jan-klod --gui` resolves `jan-klod-gui` as a sibling of itself, which is true
# in a bundle and false in a developer tree, because two workspaces mean two
# `target/` directories. Copying it beside the host workspace's binaries makes
# the developer path and the shipped path find the shell the same way, instead
# of teaching the client a second rule that only a checkout would ever use.
gui:
	$(MAKE) -C $(GUI_DIR) stage

# Self-contained release bundle: core binary + staged guests + config + README,
# as dist/jan-klod-<version>-<os>-<arch>.tar.gz.
BUNDLE_OUT ?= $(abspath dist)

# `make bundle DIST=coding` builds from a named distribution — a guest list and
# a config under scripts/distributions/. With no DIST this keeps doing exactly
# what it did before: the repository's own config.yaml and the whole of ext/.
# That default is asserted by host/tests/it/bundle_distributions.rs rather than
# left as an intention.
DIST ?=

# `make bundle GUI=1` also ships the Tauri window, and the archive name gains
# `-gui` so it does not overwrite the one without it.
#
# **A second axis, not a fourth distribution**, and that is
# scripts/distributions/README.md's rule rather than a preference: "A
# distribution says what a jan-klod install is *for*. It is not a client choice
# — `tui` versus `gui` is how you look at the runtime, and that is orthogonal to
# what the runtime does." #143 asked for a `gui` distribution beside the other
# three; taking that literally would have duplicated `coding`'s guest list into
# a directory whose only real difference is one binary, and left two lists to
# keep in step. So `DIST` still says what the install is for and `GUI` says
# whether a window ships — `make bundle DIST=coding GUI=1` is both.
#
# Opt-in rather than always-on because it is not free: +329 packages to build,
# ~10 MB in the archive, and on Linux a webkit2gtk build dependency the other
# archives do not need (#141).
GUI ?=
GUI_BIN := $(abspath $(GUI_DIR)/target/release/jan-klod-gui)
JK_GUI_BIN := $(if $(GUI),$(GUI_BIN),)

bundle: extensions
	cd $(HOST_WS) && cargo build --release -p jan-klod-host -p jan-klod
	@if [ -n "$(GUI)" ]; then $(MAKE) -C $(GUI_DIR) release; fi
	@if [ -n "$(DIST)" ]; then 	  sh scripts/dist-stage.sh "$(DIST)" "$(EXT_DIR)" "$(BUNDLE_OUT)/.staged-$(DIST)"; 	  JK_DIST="$(DIST)" JK_GUI_BIN="$(JK_GUI_BIN)" sh scripts/bundle.sh $(HOST_WS)/target/release/jan-klod-gateway $(HOST_WS)/target/release/jan-klod 	    "$(BUNDLE_OUT)/.staged-$(DIST)" "$(abspath scripts/distributions/$(DIST)/config.yaml)" $(BUNDLE_OUT); 	else 	  JK_GUI_BIN="$(JK_GUI_BIN)" sh scripts/bundle.sh $(HOST_WS)/target/release/jan-klod-gateway $(HOST_WS)/target/release/jan-klod $(EXT_DIR) $(CONFIG) $(BUNDLE_OUT); 	fi

clippy: check-spike-deps
	$(MAKE) -C $(HOST_WS) clippy

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
	@for m in $(HOST_WS)/Cargo.toml $(EXT)/Cargo.toml $(GUI_DIR)/Cargo.toml; do \
	  cargo metadata --locked --format-version 1 --manifest-path "$$m" >/dev/null \
	    && echo "lockfile current: $$m" \
	    || { echo "stale lockfile: $$m (run: cargo update --manifest-path $$m)" >&2; exit 1; }; \
	done

# Each subtree owns its own audit/deny invocation; root fans out to all three.
#
# `src/gui` is here and not optional. It is the tree that made `deny.toml` grow
# eleven named entries (#141), so a policy run that skipped it would be checking
# every workspace except the one the policy was widened for.
audit deny:
	$(MAKE) -C $(HOST_WS) $@
	$(MAKE) -C $(EXT) $@
	$(MAKE) -C $(GUI_DIR) $@

# CycloneDX SBOM for all Rust crates. Install once: cargo install cargo-cyclonedx
#
# `src/gui` is generated too, and it is not a formality: `jan-klod-gui` is a
# binary a `-gui` archive ships, and it carries 329 packages none of the others
# do (#141). An SBOM that describes the gateway and the client but not the third
# binary in the same tarball answers the question it exists to answer — "what is
# in this release?" — with two thirds of it.
#
# `cd` per workspace because cargo-cyclonedx writes beside each manifest; the
# results are then merged into one document by `jaq` (#208).
#
# **One** glob, `src/*/*.cdx.json`, and that is not a shortcut: every crate in
# both workspaces — the five host members and `jan-klod-gui` — is a direct
# child of `src/`, so one level of wildcard reaches all six and nothing else.
# It has to be exactly one level. `src/**/...` is what was here before, and in
# `sh` (no `globstar`) `**` is just `*`, so it was never recursive; when the
# members moved out from under `src/core/` it silently stopped matching four of
# the five and the SBOM lost them. A glob that quietly describes less than it
# claims is the failure this repository keeps paying for, so the count is
# asserted rather than trusted.
sbom:
	cd $(HOST_WS) && cargo cyclonedx --format json --quiet
	cd $(GUI_DIR) && cargo cyclonedx --format json --quiet
	@n=$$(ls $(HOST_WS)/*/*.cdx.json | wc -l | tr -d ' '); \
	  test "$$n" -eq 6 \
	    || { echo "sbom: expected 6 per-crate documents, found $$n:" >&2; \
	         ls $(HOST_WS)/*/*.cdx.json >&2; exit 1; }
	cat $(HOST_WS)/*/*.cdx.json | \
	  jaq -s '{bomFormat:.[0].bomFormat,specVersion:.[0].specVersion,version:1,serialNumber:.[0].serialNumber,components:[.[].components//[]|.[]]}' \
	  > sbom.cdx.json

# The npm leg (#120). A named target rather than a line inlined below, and that
# is not a style choice: the inlined supervisor line further down has never run
# on a CI runner, because CI calls the legs by name and never calls
# `supply-chain` itself (#130). A gate that does not run looks exactly like a
# gate that passes.
#
# Advisories only. `npm audit` is the analogue of `cargo audit`; there is no
# analogue here of `cargo deny`'s licence and source policy, so src/web's
# dependencies are checked for known vulnerabilities and for nothing else.
#
# `npm ci --dry-run` first, and it is load-bearing rather than belt-and-braces:
# `npm audit` reads package-lock.json and **does not notice** a package.json
# naming a dependency the lockfile has never seen — it reports "found 0
# vulnerabilities" and exits 0 while a new dependency goes entirely unscanned.
# This is what `make lockfile` buys for Cargo: the audited tree has to be the
# tree that builds. `--dry-run` because resolving the tree is the whole point;
# installing it is not, and `npm audit` needs no node_modules.
#
# DO NOT add --omit=dev or --production here. Every one of src/web's 29
# packages is a devDependency (esbuild + typescript; there are no runtime
# dependencies at all), so omitting them makes this command scan an empty set,
# report "found 0 vulnerabilities" and exit 0 forever. The usual advice — audit
# what ships, skip the build tools — inverts this gate's entire purpose: a
# compromised build-time package runs with the privileges of whoever builds.
web-supply-chain:
	cd $(WEB) && npm ci --dry-run && npm audit

# The supervisor's own Go leg (#130). A named target, and the name is the whole
# fix: this was the last line of `supply-chain` below, and CI calls the legs by
# name and never calls `supply-chain` itself — so the vulnerability scan for the
# repository's only production Go binary ran exactly when somebody typed
# `make supply-chain` by hand. Inlining made it invisible to the workflow and
# nothing failed, because a gate that does not run looks exactly like a gate
# that passes. Extracting it without also calling it by name in `ci.yml` would
# have reproduced the same bug in tidier form, which is why the two happen
# together.
#
# `go run …@$(GOVULNCHECK_VERSION)` rather than a `govulncheck` off `PATH`,
# matching src/extensions' `go-supply-chain`: no runner has govulncheck
# installed, and the neighbouring leg already resolves the tool this way. That
# answered #131's second question — the supervisor does use the same call — and
# #131 then closed the first one by pinning the version both sites share, in
# versions.mk.
#
# `src/supervisor` has no `go.sum`: it is a dependency-free binary, so
# `go mod verify` passes trivially and the real work here is govulncheck's
# **standard library** scan, which is what a Go toolchain CVE would land in.
supervisor-supply-chain: export GOFLAGS = -mod=readonly
supervisor-supply-chain:
	cd $(SUPERVISOR) && go mod verify && go run golang.org/x/vuln/cmd/govulncheck@$(GOVULNCHECK_VERSION) ./...

# License/advisory/source policy + RUSTSEC audit (host + guests), Go vuln
# scan (guests + supervisor), the npm advisory scan, and the SBOM.
supply-chain: deny audit sbom web-supply-chain supervisor-supply-chain
	$(MAKE) -C $(EXT) go-supply-chain

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
# These are modules of one test binary (src/host/tests/it/main.rs), run by
# --test name filter. A typo'd filter can still match other modules and exit 0,
# so each name is checked against the filesystem before the run.
HARNESS_MODULES := component_harness agent_loop persistence api_rest rpc telegram \
                   host_fs host_process tool_fleet tool_wiring guardrails client_surface contributions_rest
harness: export JK_REQUIRE_GUESTS = 1
harness: check-spike-deps extensions
	@for m in $(HARNESS_MODULES); do \
	  test -f $(HOST_WS)/host/tests/it/$$m.rs \
	    || { echo "harness: no module $$m.rs in $(HOST_WS)/host/tests/it/" >&2; exit 1; }; \
	done
	cd $(HOST_WS) && cargo nextest run -p jan-klod-host --features jan-klod-host/integration $(addsuffix ::,$(HARNESS_MODULES))

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
GATE_LIST := $(HOST_WS)/target/gate-tests.txt

gate: export JK_REQUIRE_GUESTS = 1
gate: check-spike-deps extensions
	@mkdir -p $(dir $(GATE_LIST))
	@cd $(HOST_WS) && cargo nextest list -p jan-klod-host --features jan-klod-host/integration --bins --tests --color never > target/gate-tests.txt
	@n=$$(grep -c '^jan-klod-host::it ' $(GATE_LIST) || true); \
	  echo "gate: integration suite (jan-klod-host::it) resolves $$n test(s)"; \
	  if [ "$$n" -eq 0 ]; then \
	    echo "gate: the integration suite compiled zero tests - the --features integration gate is broken" >&2; \
	    echo "gate: what nextest listed (first 20 lines):" >&2; \
	    head -20 $(GATE_LIST) >&2; \
	    exit 1; \
	  fi
	cd $(HOST_WS) && cargo nextest run --workspace --features jan-klod-host/integration --no-fail-fast
	cd $(HOST_WS) && cargo test --doc --workspace

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
	cargo fmt --manifest-path $(HOST_WS)/Cargo.toml --all -- --check
	cargo fmt --manifest-path $(EXT)/Cargo.toml --all -- --check
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) -C $(HOST_WS) check
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
#
# All three clippys run here and none of them runs in `gate`, which stays
# inside the host workspace. `clippy-guests` came with #205 and `clippy-gui`
# with #207, both filed for the same reason: a strict lint policy that only
# CI runs is a policy a developer meets after pushing. `clippy-gui` compiles
# the Tauri workspace, which is 329 packages nothing else here needs — that
# cost belongs on a push and not on a commit.
#
# **This does not make a green push imply a green CI**, and it is worth
# knowing why. `src/host/tests/it/sandbox_landlock.rs` is
# `#![cfg(target_os = "linux")]`, so on macOS clippy never compiles it and
# never reads it; CI runs Linux and does. No target added here closes that,
# because the gap is the platform rather than the target list (#207).
gate-push: export JK_REQUIRE_GUESTS = 1
gate-push:
	$(MAKE) -C $(EXT) spike-deps
	rm -rf $(EXT_DIR)
	$(MAKE) -C $(EXT) all
	$(MAKE) test-guests
	$(MAKE) clippy
	$(MAKE) clippy-guests
	$(MAKE) clippy-gui
	$(MAKE) gate
	$(MAKE) registry-index-drift
	$(MAKE) lockfile
	$(MAKE) deny
	$(MAKE) audit
	$(MAKE) -C $(EXT) go-supply-chain
	$(MAKE) sbom

# Boot the real core against config.yaml: resolve extensions against ext/,
# compile present components, run their lifecycle, print the boot plan.
run:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod-host -- $(CONFIG) $(EXT_DIR)

# Serve the loop over the host-side REST surface. Uses live host-http, so the
# enabled provider needs its api-key env. Override BIND=host:port.
BIND ?= 127.0.0.1:8787
serve:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod-host -- serve $(CONFIG) $(EXT_DIR) $(BIND)

# TUI client — connects to the gateway (auto-starting it if not running).
# Override ADDR=host:port and SESSION=id.
ADDR ?= 127.0.0.1:8787
SESSION ?= cli
chat:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod -- $(ADDR) $(SESSION)

# The same client, in a window. `gui` first so the shell is staged beside the
# binary `cargo run` produces — without it `--gui` correctly reports that the
# shell is not installed, which is accurate and unhelpful as a dev loop.
chat-gui: gui
	cd $(HOST_WS) && cargo run --quiet -p jan-klod -- --gui --addr $(ADDR)

# Telegram bot (headless). Needs TELEGRAM_BOT_TOKEN + network.
chat-telegram:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod-host -- telegram $(CONFIG) $(EXT_DIR)

# Drives a provider's llm-provider.complete path against a live endpoint.
# Needs the provider's api-key env (e.g. OPENAI_API_KEY) — real, billed call.
probe:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod-host --features examples --example provider_probe -- $(CONFIG) $(EXT_DIR)

# Resolve config.yaml and print the extension plan (each instance -> wasm).
config:
	cd $(HOST_WS) && cargo run --quiet -p jan-klod-config --features examples --example dump -- $(CONFIG)

# One-time developer setup: cargo supply-chain plugins, git hooks, and the WIT
# deps that only `wkg` can fetch.
setup:
	cargo install cargo-audit --version $(CARGO_AUDIT_VERSION)
	cargo install cargo-cyclonedx --version $(CARGO_CYCLONEDX_VERSION)
	cargo install cargo-nextest --version $(CARGO_NEXTEST_VERSION)
	cargo install cargo-deny --version $(CARGO_DENY_VERSION)
	cargo install wasm-tools --version $(WASM_TOOLS_VERSION)
	cargo install wkg --version $(WKG_VERSION)
	cargo install jaq --version $(JAQ_VERSION)
	$(MAKE) -C $(EXT) spike-deps
	$(MAKE) install-hooks

install-hooks:
	git config core.hooksPath .github/hooks

clean:
	$(MAKE) -C $(HOST_WS) clean
	$(MAKE) -C $(EXT) clean
	$(MAKE) -C $(GUI_DIR) clean
	rm -rf $(CACHE_DIR)
