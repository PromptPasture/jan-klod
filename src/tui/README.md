# `src/tui` — the terminal client

The client you type into. **Separate process** (not extension): spawns `jan-klod-gateway rpc` and speaks newline-delimited JSON-RPC over pipes, or drives an existing gateway. Depends only on `jan-klod-protocol` wire contract (serde only, by design).

## Dependencies, and why each one is here

| Crate | Why |
|---|---|
| `jan-klod-protocol` | The wire contract. Serde-only by design (#41), which is what lets a client depend on it without pulling in the core or Wasmtime. |
| `serde_json` | The framing on both transports. |
| `ratatui` 0.30 | The terminal UI, for the `tui` mode only. Bundles the crossterm backend and event types, used via `ratatui::crossterm`. The pure `App` model in `app.rs` needs none of it and is unit-tested without a terminal. |
| `tiny_http` *(dev)* | The round-trip test stands up a canned HTTP server to drive the client against. |

Three runtime dependencies—the budget. Additions are costed rather than waved through (next section shows how).

## Declined: a Markdown renderer and a syntax highlighter

Phase 19 renders Markdown with highlighted code ([#99]). `tui-markdown` 0.3.9 and `syntect` 5.3.0 are obvious candidates. **Both declined**, measured in [#146] against baseline of `ratatui` + `serde_json` (184 packages):

| Option | Packages | Δ | `cargo deny`, root policy unmodified |
|---|---:|---:|---|
| `tui-markdown` 0.3.9 | 234 | **+50** | **advisories FAILED** |
| `syntect` 5.3.0 | 208 | +24 | **advisories FAILED** |
| `pulldown-cmark` 0.13, `default-features = false` | 186 | **+2** | ok |

**The advisories decide it.** `syntect` brings RUSTSEC-2025-0141 (`bincode` unmaintained) and RUSTSEC-2024-0320 (`yaml-rust`, no upgrade). `tui-markdown` inherits both via `syntect`. Taking either widens root `deny.toml` — and a gate widened to let something through stops being a gate.

Three further findings, none of which are visible from a crate page:

- **`syntect` is not an alternative beside `tui-markdown`; it is inside it.**
  Resolving both together still costs 234 packages.
- **`tui-markdown` ships test frameworks as normal deps.** `rstest` and `pretty_assertions` in normal tree (not dev), so every dependent compiles unused test code.
- **Highlights through ANSI.** `ansi-to-tui` (normal dep): syntect → escape codes → ratatui spans. Design rule: highlighter theme ignored, tokens map to ramp. But ANSI→reparse means colors are syntect's theme by construction.

**What is used instead:** `pulldown-cmark` with default features off (headings, bold/italic, inline code, lists, block quotes, fenced blocks, links). Same parser `tui-markdown` uses, minus 48 crates. Highlighting left separate (nothing measured passed policy); [#149] answers it independently.

## Declined again: syntax highlighting

Phase 19 asked for highlighted fenced blocks ([#99]). **Not done—cost, not difficulty** ([#149]). Against baseline (ratatui + serde_json + pulldown-cmark): 186 packages, 191 MB target/:

| Candidate | Packages | Δ | `target/` | `cargo deny`, root policy unmodified |
|---|---:|---:|---:|---|
| `syntect` 5.3 | 208 | +24 | — | **advisories FAILED** (RUSTSEC-2025-0141, RUSTSEC-2024-0320) |
| `synoptic` 2 | 190 | +4 | — | **licences FAILED** — `char_index` is MPL-2.0 |
| `tree-sitter-highlight` 0.25 | 194 | +8 | 327 MB | ok |
| `inkjet` 0.11 | 204 | +18 | **731 MB** | ok |

Policy-passing options cost most to build. `tree-sitter-highlight` adds **136 MB** before a grammar (highlights nothing solo; each language adds vendored C). `inkjet` bundles grammars (one dep vs. dozen), hence **540 MB** + 156s CPU.

A 3-dep terminal client doesn't trade 540 MB for code color. Build footprint is first-class here.

**Fenced blocks marked by surface + indent only** (tagged or untagged). Test `markdown::tests::no_fence_carries_syntax_colour_tagged_or_not` pins this; future highlighting must change it deliberately.

## Not declined, not needed: a diff engine

[#156] renders unified diffs; [#100] allows `similar` only if client computes one. **It doesn't.**

`tool-git` allowlists five read-only subcommands; `op=diff` runs `git diff --no-ext-diff --no-textconv`, returning git's stdout (trimmed, capped). `op=show` carries diff in same format. Content arrives **already unified-diff text**; client parses/renders (vs. computes). Third dependency question answered without a package.

`tool-edit` returns prose (`"replace applied to <path> (N lines)"` + warning), not diff. No other tool produces diff-shaped output.

Two facts about that text the renderer has to hold, both from the guests rather
than from git:

- **It can arrive cut.** Every guest passes its output through
  `guest_fs::truncate`, which appends `…[truncated: N bytes omitted]` on its own
  line past the cap. A long diff therefore reaches the client with a hunk whose
  body is shorter than its header claims, and a trailing line that is not diff
  syntax at all.
- **Empty diff → `(no output)`.** `git::render` substitutes for zero exit; "nothing changed" lands on plain-text (vs. diff-with-no-hunks).
- **`--no-ext-diff --no-textconv` hardens (not formats)**, but one consequence: always git's built-in format (vs. driver-specific), one shape to parse.

## Colour and glyphs come from `theme.rs`, and only from there

`src/tui/src/theme.rs` owns grey ramp (10 steps), accents (4), glyphs, capability detection. Ask for **roles** — `body()`, `border_active()`, `warning()` — or `Glyph`, never `Color` or `✓`.

This is enforced rather than requested: Test `no_colour_literal_survives_outside_this_module` greps crate src/ and tests/, fails if literal appears elsewhere. Also asserts ≥7 scanned files (grep-nothing passes for free).

Two limits worth knowing before designing against it:

- **`muted` lacks light-terminal contrast floor** (3.05:1 bg, 2.41:1 surface). Light ramp has two AA tiers (not three); no shared step fixes both. [#135] holds decision.
- **Grey=structure, color=meaning.** Contrast target: borders/glyphs carrying meaning ≥ 3:1. Accents + focused border gated; grey separators not.

## Tests

`app.rs` is pure model, tested without terminal (pattern for Phase 19 slices). Rendering = App + width, so viewport/wrap/block testable without drawing. `tests/` holds model, frame-parsing, round-trip suites.
