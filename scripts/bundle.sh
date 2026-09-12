#!/usr/bin/env sh
# Assemble a self-contained jan-klod bundle: core binary + selected guests +
# pre-filled config + README, tarred as jan-klod-<version>-<os>-<arch>.tar.gz.
#
# Usage: bundle.sh <core-binary> <ext-dir> <config> <out-dir>
#
# Optional environment:
#   JK_DIST     the distribution's name, which the archive name carries
#   JK_GUI_BIN  path to `jan-klod-gui`; when set, the window ships too and the
#               archive name carries `-gui`
set -eu

GATEWAY_BIN="$1"
UI_BIN="$2"
EXT_DIR="$3"
CONFIG="$4"
OUT="$5"

VERSION="${JK_VERSION:-0.1.0}"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
# A distribution's archive carries its name, or three `make bundle DIST=` runs
# would overwrite one file and the third would look like all of them.
#
# `-gui` is a second, independent suffix rather than a fourth distribution, and
# that is scripts/distributions/README.md's rule rather than a preference: "a
# distribution says what a jan-klod install is *for*. It is not a client choice
# — `tui` versus `gui` is how you look at the runtime". So the window rides
# whichever distribution the user wanted, and `coding` and `coding-gui` differ by
# one binary rather than by a duplicated guest list somebody has to keep in step.
SUFFIX="${JK_DIST:+-$JK_DIST}${JK_GUI_BIN:+-gui}"
NAME="jan-klod-${VERSION}-${OS}-${ARCH}${SUFFIX}"
DIR="${OUT}/${NAME}"

rm -rf "$DIR"
mkdir -p "$DIR/ext"
cp "$GATEWAY_BIN" "$DIR/jan-klod-gateway"
cp "$UI_BIN" "$DIR/jan-klod"
cp "$CONFIG" "$DIR/config.yaml"
# The GUI shell, when this is a `-gui` archive. Beside `jan-klod` rather than in
# a subdirectory because that is how `jan-klod --gui` finds it: sibling of the
# running executable, then PATH (jan_klod::sibling_bin).
if [ -n "${JK_GUI_BIN:-}" ]; then
	[ -f "$JK_GUI_BIN" ] || {
		echo "bundle: JK_GUI_BIN is set but '$JK_GUI_BIN' does not exist" >&2
		echo "bundle: run 'make -C src/gui release' first" >&2
		exit 1
	}
	cp "$JK_GUI_BIN" "$DIR/jan-klod-gui"
fi
# Exclude tool-escape-probe: a test-only sandbox-escape instrument, inert
# unless enabled, but a bad look to ship in a release.
# Each component's manifest travels with it: the runtime refuses a component
# that ships without one, so a bundle carrying only `.wasm` files would install
# and then refuse to boot.
if [ -d "$EXT_DIR" ]; then
	find "$EXT_DIR" -maxdepth 1 \
		\( -name '*.wasm' -o -name '*.manifest.toml' \) \
		! -name 'tool-escape-probe.*' \
		-exec cp {} "$DIR/ext/" \;
fi

# Verify the assembled bundle before tarring: a missing guest degrades silently
# (the runtime just skips what it can't find), so check with the real resolver
# instead of shipping a bundle that's quietly less capable than its config claims.
#
# API key env vars are placeholders — verify never calls a provider, but config
# expansion fails on an unset key.
( cd "$DIR" && OPENAI_API_KEY="${OPENAI_API_KEY:-unset}" \
	ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-unset}" \
	GROQ_API_KEY="${GROQ_API_KEY:-unset}" \
	SEARCH_API_KEY="${SEARCH_API_KEY:-unset}" \
	./jan-klod-gateway verify config.yaml ext >/dev/null ) || {
	echo "bundle: FAILED — the assembled bundle is incomplete (see above)" >&2
	echo "bundle: run 'make ext' so every guest the config enables is staged" >&2
	exit 1
}

cat > "$DIR/README.md" <<EOF
# jan-klod ${VERSION} (${OS}-${ARCH})

Self-contained bundle: the core binary, its guest extensions (\`ext/\`), and a
pre-filled \`config.yaml\`. Persistence and the REST surface are built into the
core — no separate store/REST guests.

## Run

\`\`\`sh
./jan-klod my-session    # starts the gateway automatically if not running
\`\`\`

Or manually:

\`\`\`sh
./jan-klod-gateway serve config.yaml ext 127.0.0.1:8787
./jan-klod 127.0.0.1:8787 my-session
\`\`\`
EOF

# The window's own section, only in archives that carry it.
if [ -n "${JK_GUI_BIN:-}" ]; then
	cat >> "$DIR/README.md" <<'EOF'

## The window

```sh
./jan-klod --gui         # starts the gateway if needed, then opens a window
```

Same front-end the browser gets, in a native window over the system webview —
no bundled browser. `--gui` takes `--addr host:port` like the other modes and
no session id: the page chooses its own.

On Linux the webview is a system package rather than part of the OS. If the
window does not open, install it:

```sh
sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1
```
EOF
fi

tar -czf "${OUT}/${NAME}.tar.gz" -C "$OUT" "$NAME"
echo "bundle: ${OUT}/${NAME}.tar.gz"
