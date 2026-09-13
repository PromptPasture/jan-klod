# `jan-klod-gui` — the desktop window

Tauri 2 window around **the web client the core serves**. Not a third codebase: `src/web` builds, `src/core` serves at `/`, this opens a webview. Vision decision 5.

```sh
make gui       # build, stage beside host binaries
jan-klod --gui # spawn-or-attach gateway, open window
```

## Why this is its own cargo workspace

Tauri cost, measured in [#141] **before** this was written, against host baseline of 406 packages:

| | |
|---|---:|
| Packages added to this repository, after dedup | **+256** |
| `src/Cargo.lock` if this were a member | 406 → **663** |
| Clean release build (this crate alone) | 57.6 s wall, **332 CPU-s**, 838 MB `target/` |
| Clean debug build | 34.2 s wall, 152 CPU-s, 768 MB |
| Release binary | **9.6 MB** |
| `deny.toml` entries required | **11** |

That table is the original measurement and is deliberately not recounted. Both locks have grown since: as of 2026-09-13 the shell adds **+329**, and `src/Cargo.lock` as a member would be 406 → **735**. The build and binary rows have not been remeasured.

As a host member, those packages would build on every test/clippy/CI run—by everyone, even those never opening a window. Separate, they hide behind `make gui`. Cost is a second `target/` (like `src/extensions` already trades).

Separation **does not** buy supply-chain gate pass. Root `Makefile` targets name all three workspaces; `.github/workflows/ci.yml` has a `gui` job (builds, lints, tests under `xvfb`). The policy-widened tree doesn't escape the policy.

## The eleven entries in `deny.toml`, and why they are not a widening

`cargo deny` fails on Tauri's tree (`advisories FAILED, licenses FAILED`). `cargo audit` passes (none are vulnerabilities).

- **5 MPL-2.0 crates**: `cssparser`, `cssparser-macros`, `dtoa-short`, `selectors` (via `dom_query`) and `option-ext` (via `dirs`). Per-crate exceptions (not `allow`), so a sixth still fails. File-level copyleft: linking requires their source; imposes nothing on Apache-2.0 code.
- **6 unmaintained advisories**: `proc-macro-error` (Linux via `gtk`) and five `unic-*` crates (one advisory-db event, via `urlpattern`). All: "No safe upgrade available!"

**Unavoidable—lighter choices don't help.** `wry` (webview binding) depends on `dom_query` and `dirs` itself: 238 packages, fails same five licenses. MPL floor is system-webview, not Tauri.

Entries carry scope notes (this crate only). If shell drops, exemptions go too (rather than standing for unbuilt tree).

[#141]: https://github.com/PromptPasture/jan-klod/issues/141

## What this binary does, and what it deliberately does not

**Only a window.** No gateway spawn, address resolution, or session knowledge:

```
jan-klod-gui --url <url> [--title <title>]
```

`jan-klod --gui` handles it: `ensure_gateway` (spawn-or-attach, in `src/tui`), then launch with answering URL. Second spawn-or-attach risks finding different gateway—the bug the split avoids.

Found like the gateway: **sibling executable, then `PATH`** (`jan_klod::sibling_bin`). Release bundles satisfy this (three binaries side-by-side); dev trees don't (two workspaces). `make gui` staging fixes this (vs. a checkout-only lookup rule).

## The one thing here that is not a window

Core token lives in `sessionStorage` (`jan-klod-token`, set by `src/web/src/api.ts`). Window with existing token shouldn't ask again—so this seeds it, making the window a credential boundary.

Two rules, both tested, both cited from `docs/concepts/security-model.md`:

- **The seed checks `location.origin` first.** A Tauri initialization script runs
  in every frame the webview loads, so an unguarded one hands the token to
  whatever a page embeds.
- **Navigation off that origin is refused** and handed to the system browser, so
  the origin check is a second line rather than the only one.

Token (from environment) → JavaScript string literal → `serde_json` escapes it. Test `a_token_with_javascript_metacharacters_is_escaped` catches `format!` replacement.

## Tests, and the half of the acceptance they cannot cover

`cargo test` runs unit tests (args, token script) and `tests/smoke.rs` (real binary vs. 40-line server, WebKit webview loads it, reads token from `sessionStorage`).

**No screen checks.** Webview fetching/running proves rendering, not sighting. [#142] split them; visual check remains manual (`docs/changelog.md` records platform).

Webview needs display server. Linux without `DISPLAY`/`WAYLAND_DISPLAY` skips smoke tests; `JK_REQUIRE_GUI=1` fails (CI sets with `xvfb-run`, asserting truth).

[#142]: https://github.com/PromptPasture/jan-klod/issues/142

## Prerequisites

**macOS** — Xcode CLI tools only (webview is WKWebView, OS-provided).

**Linux** — the webview is a system package. To build:

```sh
sudo apt install libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

Runtime halves: `libwebkit2gtk-4.1-0`, `libayatana-appindicator3-1`. Missing library stops before `main`; `jan-klod --gui` reports exit + package name (vs. silent fallback—wrong signal to user).

**Windows** — untried. There is no Windows runner (see [#49]), so the release
matrix does not build one and nothing here has been verified on it.

[#49]: https://github.com/PromptPasture/jan-klod/issues/49

## The icon is a placeholder

`icons/icon.png` is a generated `>_` on slate (not brand). Required by `tauri::generate_context!` at compile time. Replacing is undecided design.
