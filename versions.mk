# Pinned tool versions shared by the root and pkgs/extensions Makefiles. A separate file because CI
# runs `make -C pkgs/extensions` directly and cannot inherit variables; a copied pin would drift (#131).
# Each pin has a twin in .github/workflows/ci.yml; keep them in sync.
# cargo-deny is why: 0.20 removed `--config` from the `check` subcommand and made
# it global, so 0.19 and 0.20 need different invocations. The other tools are pinned to the
# versions this repository was verified against.
CARGO_DENY_VERSION := 0.20.2
CARGO_AUDIT_VERSION := 0.22.2
CARGO_CYCLONEDX_VERSION := 0.5.9
CARGO_NEXTEST_VERSION := 0.9.143

# Scanners are pinned: a new release can change findings and turn an unchanged commit red.
#
# Run via `go run`, not a PATH binary, because no runner has govulncheck installed.
GOVULNCHECK_VERSION := v1.8.0

# Not a cargo plugin, but `make extensions` parses its WIT output, so it is pinned too.
WASM_TOOLS_VERSION := 1.258.0

# Pinned to match CI's install and the committed wit/wkg.lock; a newer wkg may not
# reproduce that lock resolution.
WKG_VERSION := 0.15.1

# jaq is jq in Rust, so it is cargo-installable like the other tools. Not a drop-in:
# `jaq -s` slurps each file separately, unlike `jq -s`, so multi-file merges pipe one stream.
JAQ_VERSION := 2.3.0

# Pinned. Opt-in: not installed by `make setup` (326 MB of node_modules for a 12.7 MB component).
JCO_VERSION := 1.34.0

# Opt-in like JCO_VERSION: 50 MB of toolchain for an 18.4 MB component, so `make setup` skips it.
COMPONENTIZE_PY_VERSION := 0.25.1
