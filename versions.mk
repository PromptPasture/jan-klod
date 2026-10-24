# Shared by the root and pkgs/extensions Makefiles: CI runs the latter directly and cannot inherit variables.
# Each pin has a twin in .github/workflows/ci.yml.
CARGO_DENY_VERSION := 0.20.2
CARGO_AUDIT_VERSION := 0.22.2
CARGO_CYCLONEDX_VERSION := 0.5.9
CARGO_NEXTEST_VERSION := 0.9.143

# Run via `go run`: no runner has govulncheck installed.
GOVULNCHECK_VERSION := v1.8.0

# `make extensions` parses its WIT output.
WASM_TOOLS_VERSION := 1.258.0

# A newer wkg may not reproduce wit/wkg.lock.
WKG_VERSION := 0.15.1

# Not a drop-in for jq: `jaq -s` slurps each file separately.
JAQ_VERSION := 2.3.0

# Opt-in: not installed by `make setup` (326 MB of node_modules).
JCO_VERSION := 1.34.0

# Opt-in: 50 MB of toolchain, so `make setup` skips it.
COMPONENTIZE_PY_VERSION := 0.25.1
