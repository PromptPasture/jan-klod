# `src/core/ui` — the terminal client

`jan-klod`, the client a person actually types into. It is a **separate process**
from the core, not an extension: it spawns `jan-klod-gateway rpc` and speaks
newline-delimited JSON-RPC over its pipes, or drives a gateway already listening
with `--addr`. It depends on neither the core runtime nor Wasmtime — only on the
wire contract in `jan-klod-protocol`, which carries nothing beyond serde for
exactly this reason.

## Dependencies, and why each one is here

| Crate | Why |
|---|---|
| `jan-klod-protocol` | The wire contract. Serde-only by design (#41), which is what lets a client depend on it without pulling in the core or Wasmtime. |
| `serde_json` | The framing on both transports. |
| `ratatui` 0.30 | The terminal UI, for the `tui` mode only. Bundles the crossterm backend and event types, used via `ratatui::crossterm`. The pure `App` model in `app.rs` needs none of it and is unit-tested without a terminal. |
| `tiny_http` *(dev)* | The round-trip test stands up a canned HTTP server to drive the client against. |

Three runtime dependencies. That is the budget, and additions are costed against
it rather than waved through — the next section is what that looks like in
practice.

## Declined: a Markdown renderer and a syntax highlighter

Phase 19's chat surface renders assistant text as Markdown with highlighted code
blocks ([#99](https://github.com/PromptPasture/jan-klod/issues/99)).
`tui-markdown` 0.3.9 and `syntect` 5.3.0 were the obvious candidates.
**Both are declined**, measured in
[#146](https://github.com/PromptPasture/jan-klod/issues/146) against a baseline
of `ratatui` + `serde_json` (184 resolved packages):

| Option | Packages | Δ | `cargo deny`, root policy unmodified |
|---|---:|---:|---|
| `tui-markdown` 0.3.9 | 234 | **+50** | **advisories FAILED** |
| `syntect` 5.3.0 | 208 | +24 | **advisories FAILED** |
| `pulldown-cmark` 0.13, `default-features = false` | 186 | **+2** | ok |

**The advisories decide it.** `syntect` brings RUSTSEC-2025-0141 (`bincode`
unmaintained) and RUSTSEC-2024-0320 (`yaml-rust` unmaintained, and cargo-deny
reports *"No safe upgrade is available!"*). `tui-markdown` depends on `syntect`
directly, so it inherits both. Taking either means adding exceptions to the root
`deny.toml` — and a supply-chain gate widened to let something through has
stopped being a gate.

Three further findings, none of which are visible from a crate page:

- **`syntect` is not an alternative beside `tui-markdown`; it is inside it.**
  Resolving both together still costs 234 packages.
- **`tui-markdown` ships two test frameworks as normal dependencies.** `rstest`
  and `pretty_assertions` are in the *normal* tree, not the dev tree, so every
  dependent compiles a test framework it will never call.
- **It highlights through ANSI.** `ansi-to-tui` is a normal dependency: the
  pipeline is syntect → escape codes → parsed back into ratatui spans. The
  design system's rule is that *the highlighter's own theme is ignored* and
  token classes map onto the ramp — but a library that renders to ANSI and
  reparses it owns that step, so the colours are syntect's theme by
  construction.

**What is used instead:** `pulldown-cmark` with default features off, and the
subset #99 names — headings, bold/italic, inline code, bullet and ordered lists,
block quotes, fenced blocks, links. That is the same parser `tui-markdown`
itself uses, without the 48 crates wrapped around it. Syntax highlighting was
left as a separate question, since nothing measured here passed the policy —
[#149](https://github.com/PromptPasture/jan-klod/issues/149) answered it on its
own terms, and the next section is that answer.

## Declined again: syntax highlighting

Phase 19 asked for fenced code blocks with a language tag to be highlighted
([#99](https://github.com/PromptPasture/jan-klod/issues/99)). **They are not**,
and the reason is cost rather than difficulty
([#149](https://github.com/PromptPasture/jan-klod/issues/149)). Measured against
a baseline of `ratatui` + `serde_json` + `pulldown-cmark` — 186 packages,
191 MB of `target/`:

| Candidate | Packages | Δ | `target/` | `cargo deny`, root policy unmodified |
|---|---:|---:|---:|---|
| `syntect` 5.3 | 208 | +24 | — | **advisories FAILED** (RUSTSEC-2025-0141, RUSTSEC-2024-0320) |
| `synoptic` 2 | 190 | +4 | — | **licences FAILED** — `char_index` is MPL-2.0 |
| `tree-sitter-highlight` 0.25 | 194 | +8 | 327 MB | ok |
| `inkjet` 0.11 | 204 | +18 | **731 MB** | ok |

The two that pass the policy are the two that cost the most to build.
`tree-sitter-highlight` adds **136 MB** of build output *before a single
grammar* — it highlights nothing on its own, and every language is another
vendored C grammar on top. `inkjet` bundles the grammars, which is why it is one
dependency rather than a dozen, and why it is **540 MB** and 156 CPU-seconds.

For a terminal chat client whose entire crate carries three runtime
dependencies, that is not a trade worth making for colour in a code block. This
repository has treated build footprint as first-class before.

**So a fenced block is marked by its surface and its indent, and by nothing
else** — tagged or untagged, the same. `markdown::tests::
no_fence_carries_syntax_colour_tagged_or_not` pins that, so a future slice that
wants highlighting has to come back and change it deliberately rather than
discover the decision by its absence.

## Colour and glyphs come from `theme.rs`, and only from there

`src/core/ui/src/theme.rs` owns the ten-step grey ramp, the four accents, the
glyph vocabulary and terminal capability detection
([#128](https://github.com/PromptPasture/jan-klod/issues/128)). Ask it for a
**role** — `body()`, `border_active()`, `warning()` — and for a `Glyph`, never
for a `Color` literal or a `✓`.

This is enforced rather than requested:
`theme::tests::no_colour_literal_survives_outside_this_module` greps the crate's
own `src/` and `tests/` and fails the build if a literal reappears anywhere
outside that module. It also asserts it scanned at least seven files, because a
grep that greps nothing passes for the same reason as one that finds nothing.

Two limits worth knowing before designing against it:

- **`muted` has no contrast floor on a light terminal** — 3.05:1 on the
  background, 2.41:1 on a raised surface. The light ramp carries two AA text
  tiers, not three, and no single shared step can fix that;
  [#135](https://github.com/PromptPasture/jan-klod/issues/135) holds the
  decision about what to do instead.
- **Grey carries structure, colour carries meaning.** The contrast target is
  "any border or glyph *that carries meaning* ≥ 3:1", so the accents and the
  focused border are gated and the grey separators deliberately are not.

## Tests

`app.rs` is a pure model and is tested without a terminal, which is the shape
every later Phase 19 slice follows: rendering is a function of `App` plus a
width, so a viewport, a wrap or a Markdown block can be asserted on without
drawing anything. `tests/` holds the model, frame-parsing and round-trip suites.
