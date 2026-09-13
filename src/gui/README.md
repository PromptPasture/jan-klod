# `jan-klod-gui` — the desktop window

A Tauri 2 window around **the web client the core already serves**. Not a third
client codebase: `src/web` builds the page, `src/core` serves it at `/`, and this
opens a system webview pointed at that address. Vision decision 5.

```sh
make gui            # build it, staged beside the host workspace's binaries
jan-klod --gui      # spawn-or-attach a gateway, then open the window
```

## Why this is its own cargo workspace

Because of what Tauri costs, measured in [#141] **before** any of this was
written, against a baseline of the host workspace at 406 packages:

| | |
|---|---:|
| Packages added to this repository, after dedup | **+256** |
| `src/Cargo.lock` if this were a member | 406 → **663** |
| Clean release build (this crate alone) | 57.6 s wall, **332 CPU-s**, 838 MB `target/` |
| Clean debug build | 34.2 s wall, 152 CPU-s, 768 MB |
| Release binary | **9.6 MB** |
| `deny.toml` entries required | **11** |

As a member of the host workspace, those 256 packages would be resolved and built by
every `cargo test`, every `cargo clippy --workspace` and every CI run — by
everyone, including everyone who never opens a window. Separate, they are behind
`make gui` and nothing else. The cost of that separation is a second `target/`
directory, which is the same trade `src/extensions` already makes.

What separation explicitly does **not** buy is a pass on the supply-chain gates.
The root `Makefile`'s `lockfile`, `deny` and `audit` targets each name all three
workspaces, and `.github/workflows/ci.yml` has a `gui` job that builds, lints and
tests this one under `xvfb`. The tree the policy was widened for is not the one
that escapes the policy.

## The eleven entries in `deny.toml`, and why they are not a widening

`cargo deny` fails on Tauri's tree under the unmodified root policy:
`advisories FAILED, licenses FAILED`. `cargo audit` passes — none of it is a
vulnerability.

- **5 MPL-2.0 crates**: `cssparser`, `cssparser-macros`, `dtoa-short`,
  `selectors` (all via `dom_query`) and `option-ext` (via `dirs`). Named as
  per-crate `exceptions` rather than adding `MPL-2.0` to `allow`, so a sixth MPL
  crate appearing anywhere still fails the gate. MPL-2.0 is file-level copyleft:
  linking these unmodified obliges us to make *their* source available and
  imposes nothing on jan-klod's Apache-2.0 code.
- **6 unmaintained advisories**: `proc-macro-error` (Linux only, via `gtk`) and
  the five `unic-*` crates, which are one advisory-db event — the whole
  `rust-unic` project — arriving through `urlpattern`. All say "No safe upgrade
  is available!"

**None of this is avoidable by choosing something lighter than Tauri.** `wry`,
the webview binding underneath it, depends on `dom_query` and `dirs` *itself*:
measured at 238 packages on its own, it still fails the same five licences. The
MPL floor is in the system-webview layer, not in Tauri.

The entries carry a scope note saying they exist for this crate alone. If the
shell is ever dropped, they go with it rather than remaining as six standing
exemptions for a tree nobody builds.

[#141]: https://github.com/PromptPasture/jan-klod/issues/141

## What this binary does, and what it deliberately does not

It is **only a window**. It does not spawn a gateway, resolve an address, or
know what a session is:

```
jan-klod-gui --url <url> [--title <title>]
```

`jan-klod --gui` does the rest — `ensure_gateway` (spawn-or-attach, already in
`src/tui`), then launch this with a URL that is already answering. A second
copy of spawn-or-attach would eventually find a different gateway than the REST
path starts, which is the bug that split was drawn to avoid.

It is found the way the gateway is: **sibling of the running executable, then
`PATH`** (`jan_klod::sibling_bin`). A release bundle satisfies that by putting
the three binaries side by side; a developer tree does not, because two
workspaces mean two `target/` directories — which is what `make gui`'s staging
step exists to fix, rather than teaching the client a second lookup rule that
only a checkout would ever use.

## The one thing here that is not a window

The core's token lives in `sessionStorage` under `jan-klod-token`, where
`src/web/src/api.ts` puts it after prompting. A window launched by a client that
*already has* the token should not make the user retype it — so this seeds it,
and that makes the window a credential boundary.

Two rules, both tested, both cited from `docs/concepts/security-model.md`:

- **The seed checks `location.origin` first.** A Tauri initialization script runs
  in every frame the webview loads, so an unguarded one hands the token to
  whatever a page embeds.
- **Navigation off that origin is refused** and handed to the system browser, so
  the origin check is a second line rather than the only one.

A token is a credential from the environment that ends up inside a JavaScript
string literal, so `serde_json` does the escaping;
`a_token_with_javascript_metacharacters_is_escaped` is the test that would catch
someone replacing it with `format!`.

## Tests, and the half of the acceptance they cannot cover

`cargo test` runs unit tests (argument parsing, the token script) and
`tests/smoke.rs`, which starts the real binary against a forty-line `std::net`
server and asks a real WebKit webview what it loaded — including reading the
seeded token back out of `sessionStorage`.

**Nobody here looks at a screen.** A webview fetching the page and running its
script is strong evidence that a window rendered; it is not a sighting. [#142]
asked for the two to be reported separately, so the visual check stays a manual
step and `docs/changelog.md` records which platform it was actually done on.

A webview needs a display server. On Linux without `DISPLAY`/`WAYLAND_DISPLAY`
the smoke tests skip; `JK_REQUIRE_GUI=1` turns that skip into a failure, and CI
sets it alongside `xvfb-run` so the flag asserts something true.

[#142]: https://github.com/PromptPasture/jan-klod/issues/142

## Prerequisites

**macOS** — nothing beyond the Xcode command line tools. The webview is
WKWebView, part of the OS.

**Linux** — the webview is a system package. To build:

```sh
sudo apt install libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

To run, the runtime halves (`libwebkit2gtk-4.1-0`,
`libayatana-appindicator3-1`). A missing shared library stops the binary before
`main`, so `jan-klod --gui` reports the non-zero exit and names the package
rather than falling back to the TUI — a GUI that silently becomes a terminal has
told the user the wrong thing about their machine.

**Windows** — untried. There is no Windows runner (see [#49]), so the release
matrix does not build one and nothing here has been verified on it.

[#49]: https://github.com/PromptPasture/jan-klod/issues/49

## The icon is a placeholder

`icons/icon.png` is a generated 512×512 `>_` prompt mark on the web client's
slate, not a brand. It exists because `tauri::generate_context!` requires one at
compile time. Replacing it is a design decision nobody has made yet.
