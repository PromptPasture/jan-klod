//! The terminal design system: one grey ramp, four accents, and the roles that
//! name them.
//!
//! The mascot is a ninja and the palette follows from it — **the interface is
//! monochrome, and colour is information rather than decoration**. A ten-step
//! grey ramp carries every structural distinction (surface, border, primary
//! text, secondary text); the four accents appear only where they mean
//! something a shape cannot say. A screen with nothing happening on it is
//! entirely grey.
//!
//! There is one ramp, read from either end. On a dark terminal `ink-0` is the
//! background and `snow` is emphasis; on a light terminal those swap. There is
//! no second palette to keep in step.
//!
//! # Roles, not steps
//!
//! Nothing outside this module names a swatch. A caller asks for a role —
//! [`Theme::body`], [`Theme::border_active`], [`Theme::warning`] — and the theme
//! decides which step that is for the terminal in front of it. That indirection
//! is the point: eight slices are about to draw things, and without one place
//! that owns colour each of them picks its own `Color::Cyan`, which is what
//! `tui.rs` does today. Keeping the ramp private is also what stops a caller
//! quietly inventing a shade that no contrast test ever sees.
//!
//! # Capabilities are arguments, not ambient
//!
//! [`Theme::new`] takes the [`Mode`], [`Depth`] and [`GlyphSet`] it is to use.
//! [`Theme::detect`] works them out from an environment that is **handed to
//! it**, and [`Theme::from_process_env`] is the one line in the module that
//! reaches for `std::env`.
//!
//! That split is what makes the design checkable. A detector that reads the
//! process environment itself can only be tested by mutating a global, and
//! tests that mutate a global race each other; running one process per test
//! would hide that race rather than remove it. So every test here states the
//! terminal it means.

use ratatui::style::Color;

/// Which end of the ramp the background is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Dark terminal: `ink-0` is the background, `snow` is emphasis.
    Dark,
    /// Light terminal: `snow` is the background, `ink-0` is emphasis.
    Light,
    /// No colour at all — every role resolves to [`Color::Reset`], leaving the
    /// terminal's own foreground and background in place.
    ///
    /// This is what `NO_COLOR` asks for, and it is a real mode rather than a
    /// degraded one: the design already treats colour as the *second* signal,
    /// so the glyph vocabulary carries every distinction on its own.
    Mono,
}

/// How many colours the terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// 24-bit colour — the hexes below, unmodified.
    TrueColor,
    /// The xterm-256 indexed palette.
    Indexed256,
    /// The original sixteen.
    Basic16,
}

/// One colour, in each of the three depths a terminal might offer.
///
/// The downsampled forms are a **fixed map**, not a nearest-colour computation
/// at runtime: a ramp whose 256-colour rendering depends on a distance function
/// is a ramp nobody can check against a contrast target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Swatch {
    rgb: (u8, u8, u8),
    xterm: u8,
    basic: Color,
}

/// One row of the tables below.
///
/// A constructor rather than a struct literal purely so each row stays on one
/// line: rustfmt expands `Swatch { .. }` to five, and a ten-step ramp written
/// out that way stops reading as the table it is.
const fn sw(rgb: (u8, u8, u8), xterm: u8, basic: Color) -> Swatch {
    Swatch { rgb, xterm, basic }
}

impl Swatch {
    const fn resolve(self, depth: Depth) -> Color {
        match depth {
            Depth::TrueColor => Color::Rgb(self.rgb.0, self.rgb.1, self.rgb.2),
            Depth::Indexed256 => Color::Indexed(self.xterm),
            Depth::Basic16 => self.basic,
        }
    }
}

/// The ten steps, light to dark. Private: see the module docs on roles.
///
/// Indexed by [`Tone`], so all ten live here as data even where no role has
/// claimed one yet — `ink-2` is the code/tool-block surface the transcript
/// slice will want, and it is cheaper to carry it than to re-derive it.
///
/// `Basic16` collapses the ramp to the four greys the original sixteen have:
/// bright (`White`), normal (`Gray`), dim (`DarkGray`) and the background end
/// (`Black`). Four buckets for ten steps loses the fine distinctions, which is
/// the honest outcome on a sixteen-colour terminal.
///
/// `ash` is `#585D66`/240 rather than #97's original `#646A74`/242: as
/// tabulated there it gave secondary text only 3.88:1 against `chalk`, the
/// light theme's raised surface, and the contrast target does not move.
const RAMP: [Swatch; 10] = [
    sw((0xF2, 0xF3, 0xF5), 255, Color::White),    // snow
    sw((0xD6, 0xDA, 0xE0), 252, Color::White),    // chalk
    sw((0xB0, 0xB6, 0xBF), 249, Color::Gray),     // mist
    sw((0x86, 0x8C, 0x96), 245, Color::Gray),     // smoke
    sw((0x58, 0x5D, 0x66), 240, Color::DarkGray), // ash
    sw((0x3D, 0x41, 0x4A), 238, Color::DarkGray), // steel-1
    sw((0x2A, 0x2D, 0x34), 236, Color::Black),    // steel-0
    sw((0x1C, 0x1E, 0x23), 234, Color::Black),    // ink-2
    sw((0x14, 0x15, 0x18), 233, Color::Black),    // ink-1
    sw((0x0B, 0x0B, 0x0C), 232, Color::Black),    // ink-0
];

/// Names for the ten [`RAMP`] steps. Private, and that is the design.
#[derive(Debug, Clone, Copy)]
enum Tone {
    Snow = 0,
    Chalk,
    Mist,
    Smoke,
    Ash,
    Steel1,
    Steel0,
    Ink2,
    Ink1,
    Ink0,
}

/// One accent, in the two directions of the ramp.
#[derive(Debug, Clone, Copy)]
struct AccentPair {
    dark: Swatch,
    light: Swatch,
}

/// One row of [`ACCENTS`]; see [`sw`] for why these are constructors.
const fn pair(dark: Swatch, light: Swatch) -> AccentPair {
    AccentPair { dark, light }
}

/// The four accents, indexed by [`Accent`].
///
/// The xterm-256 indices are the nearest member of the indexed palette to each
/// hex, fixed here once rather than computed per frame. `Basic16` uses the
/// standard eight — blue, green, red, yellow — because those are the only
/// meanings the original sixteen can carry without ambiguity.
const ACCENTS: [AccentPair; 4] = [
    // blade — focus
    pair(
        sw((0x8F, 0xB8, 0xD4), 110, Color::Blue),
        sw((0x2C, 0x62, 0x85), 24, Color::Blue),
    ),
    // edge — added / completed
    pair(
        sw((0x7F, 0xA8, 0x6B), 107, Color::Green),
        sw((0x3F, 0x6B, 0x2E), 64, Color::Green),
    ),
    // strike — removed / failed
    pair(
        sw((0xC9, 0x7B, 0x7B), 138, Color::Red),
        sw((0x9B, 0x35, 0x35), 95, Color::Red),
    ),
    // ember — attention
    pair(
        sw((0xC9, 0xA2, 0x4A), 179, Color::Yellow),
        sw((0x8A, 0x66, 0x12), 94, Color::Yellow),
    ),
];

/// Names for the four [`ACCENTS`]. Private, like [`Tone`].
#[derive(Debug, Clone, Copy)]
enum Accent {
    Blade = 0,
    Edge,
    Strike,
    Ember,
}

/// Which of the two glyph vocabularies the terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphSet {
    /// The box-drawing, braille and tick marks of [`Glyph`]'s first column.
    Unicode,
    /// The ASCII fallback, for a terminal without them and for `--ascii`.
    Ascii,
}

/// A state the interface has to show without relying on colour.
///
/// Callers ask for `Glyph::ToolDone`, never for a literal `✓`: the theme owns
/// which vocabulary is in use, and a literal in a drawing routine is a literal
/// that never degrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// The rule down the left of a message block.
    MessageGutter,
    /// A tool that finished.
    ToolDone,
    /// A tool that failed.
    ToolFailed,
    /// A block that can be opened.
    Collapsed,
    /// A block that is open.
    Expanded,
    /// A warning.
    Warning,
    /// A `+` line in a diff.
    DiffAdded,
    /// A `-` line in a diff.
    DiffRemoved,
    /// The composer's caret.
    Caret,
}

impl Glyph {
    /// Every variant, so a caller sweeping the vocabulary cannot miss one.
    ///
    /// A `match` in [`Glyph::forms`] keeps this honest in the other direction:
    /// adding a variant without a form is a compile error.
    pub const ALL: [Self; 9] = [
        Self::MessageGutter,
        Self::ToolDone,
        Self::ToolFailed,
        Self::Collapsed,
        Self::Expanded,
        Self::Warning,
        Self::DiffAdded,
        Self::DiffRemoved,
        Self::Caret,
    ];

    /// The Unicode form and the ASCII one, in that order.
    ///
    /// `tool completed` is `*` in ASCII rather than #97's original `+`: the
    /// table gave `+` to both it and `diff added`, and under monochrome the
    /// glyph is the whole signal, so two states cannot share one. `✓` and `+`
    /// were already distinct in Unicode; only the fallback needed moving.
    #[must_use]
    pub const fn forms(self) -> (&'static str, &'static str) {
        match self {
            Self::MessageGutter => ("▍", "|"),
            Self::ToolDone => ("✓", "*"),
            Self::ToolFailed => ("✗", "x"),
            Self::Collapsed => ("▸", ">"),
            Self::Expanded => ("▾", "v"),
            Self::Warning => ("!", "!"),
            Self::DiffAdded => ("+", "+"),
            Self::DiffRemoved => ("-", "-"),
            Self::Caret => ("›", ">"),
        }
    }
}

/// The frames of the "a tool is running" spinner, Unicode then ASCII.
///
/// Not a [`Glyph`]: a spinner is identified by motion rather than by shape, so
/// it is neither a single mark nor subject to the distinctness rule the static
/// glyphs are — its ASCII frames deliberately reuse `|` and `-`, which no
/// stationary glyph could.
const SPINNER: (&[&str], &[&str]) = (
    &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    &["-", "\\", "|", "/"],
);

/// Read a terminal's background out of `COLORFGBG`, which is `fg;bg` and
/// sometimes `fg;something;bg`, so the background is the last field.
///
/// The numbers are ANSI indices: 0–6 and 8 are the dark half, 7 and 9–15 the
/// light one. Anything else — `default`, an empty string, a 256-colour index —
/// is not an answer, and `None` lets the caller fall through rather than guess.
fn background_of(colorfgbg: &str) -> Option<Mode> {
    match colorfgbg.rsplit(';').next()?.trim().parse::<u8>().ok()? {
        0..=6 | 8 => Some(Mode::Dark),
        7 | 9..=15 => Some(Mode::Light),
        _ => None,
    }
}

/// Whether a locale string names UTF-8.
///
/// Case-insensitive and hyphen-insensitive because all four of `en_US.UTF-8`,
/// `en_US.utf8`, `C.UTF-8` and `en_US.Utf-8` are in the wild.
fn is_utf8(locale: &str) -> bool {
    let normalised = locale.to_ascii_lowercase().replace('-', "");
    normalised.contains("utf8")
}

/// The design system, resolved for one terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    mode: Mode,
    depth: Depth,
    glyphs: GlyphSet,
}

impl Theme {
    /// A theme for a terminal whose capabilities are already known.
    ///
    /// All three are arguments rather than something this constructor sniffs,
    /// so a test can name the terminal it means.
    #[must_use]
    pub const fn new(mode: Mode, depth: Depth, glyphs: GlyphSet) -> Self {
        Self {
            mode,
            depth,
            glyphs,
        }
    }

    /// Resolve a theme from the environment.
    ///
    /// `lookup` is the environment, injected rather than read: a detector that
    /// calls `std::env::var` itself can only be tested by mutating a
    /// process-global, and tests that do that race each other. Running under
    /// `cargo-nextest`, one process per test, would hide the race rather than
    /// remove it, so it is not a substitute for taking the environment as an
    /// argument.
    ///
    /// `force_ascii` is the `--ascii` flag: a command-line override beats the
    /// locale, and only the locale.
    ///
    /// # Precedence
    ///
    /// Mode: `NO_COLOR` (set to anything) → `JAN_KLOD_THEME` → `COLORFGBG` →
    /// dark. Depth: `COLORTERM` → `TERM` → sixteen colours. Glyphs:
    /// `force_ascii` → `LC_ALL` / `LC_CTYPE` / `LANG` → ASCII.
    ///
    /// #97 also lists a terminal background *query* between `COLORFGBG` and the
    /// default, "where the backend supports it". This one does not: crossterm
    /// 0.29, which ratatui 0.30 bundles, has no such API — its only OSC
    /// sequences are for the clipboard. Rather than leave a stub, the chain
    /// falls through to dark, which is the documented default anyway.
    pub fn detect(lookup: impl Fn(&str) -> Option<String>, force_ascii: bool) -> Self {
        Self {
            mode: Self::detect_mode(&lookup),
            depth: Self::detect_depth(&lookup),
            glyphs: Self::detect_glyphs(&lookup, force_ascii),
        }
    }

    /// [`Theme::detect`] against this process's own environment.
    ///
    /// The one place in this module that touches `std::env`, so that everything
    /// above it stays a pure function of its arguments.
    #[must_use]
    pub fn from_process_env(force_ascii: bool) -> Self {
        Self::detect(|key| std::env::var(key).ok(), force_ascii)
    }

    fn detect_mode(lookup: &impl Fn(&str) -> Option<String>) -> Mode {
        // `NO_COLOR` wins outright, whatever it is set to — the convention is
        // presence, not value, and a user who set it has already answered every
        // question below.
        if lookup("NO_COLOR").is_some() {
            return Mode::Mono;
        }
        match lookup("JAN_KLOD_THEME").as_deref() {
            Some("dark") => return Mode::Dark,
            Some("light") => return Mode::Light,
            Some("mono") => return Mode::Mono,
            // An unrecognised value is not an instruction. Falling through beats
            // failing: a typo in a shell profile should not stop the client.
            _ => {}
        }
        lookup("COLORFGBG")
            .as_deref()
            .and_then(background_of)
            .unwrap_or(Mode::Dark)
    }

    fn detect_depth(lookup: &impl Fn(&str) -> Option<String>) -> Depth {
        if matches!(lookup("COLORTERM").as_deref(), Some("truecolor" | "24bit")) {
            return Depth::TrueColor;
        }
        if lookup("TERM").is_some_and(|term| term.contains("256color")) {
            return Depth::Indexed256;
        }
        Depth::Basic16
    }

    fn detect_glyphs(lookup: &impl Fn(&str) -> Option<String>, force_ascii: bool) -> GlyphSet {
        if force_ascii {
            return GlyphSet::Ascii;
        }
        // POSIX precedence: the first of the three that is set decides, even if
        // it decides against UTF-8. `LC_ALL=C` with `LANG=en_US.UTF-8` means the
        // user asked for C.
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(lookup);
        match locale {
            Some(locale) if is_utf8(&locale) => GlyphSet::Unicode,
            _ => GlyphSet::Ascii,
        }
    }

    /// Which end of the ramp this theme reads from.
    #[must_use]
    pub const fn mode(self) -> Mode {
        self.mode
    }

    /// How many colours this theme will emit.
    #[must_use]
    pub const fn depth(self) -> Depth {
        self.depth
    }

    /// Which glyph vocabulary this theme will draw with.
    #[must_use]
    pub const fn glyphs(self) -> GlyphSet {
        self.glyphs
    }

    /// The mark for a state, in whichever vocabulary this terminal has.
    #[must_use]
    pub const fn glyph(self, glyph: Glyph) -> &'static str {
        let (unicode, ascii) = glyph.forms();
        match self.glyphs {
            GlyphSet::Unicode => unicode,
            GlyphSet::Ascii => ascii,
        }
    }

    /// The spinner's frames, in whichever vocabulary this terminal has.
    #[must_use]
    pub const fn spinner(self) -> &'static [&'static str] {
        match self.glyphs {
            GlyphSet::Unicode => SPINNER.0,
            GlyphSet::Ascii => SPINNER.1,
        }
    }

    /// Resolve a ramp role, given the step each direction uses.
    const fn ramp(self, dark: Tone, light: Tone) -> Color {
        match self.mode {
            Mode::Mono => Color::Reset,
            Mode::Dark => RAMP[dark as usize].resolve(self.depth),
            Mode::Light => RAMP[light as usize].resolve(self.depth),
        }
    }

    /// Resolve an accent.
    const fn accent(self, accent: Accent) -> Color {
        match self.mode {
            Mode::Mono => Color::Reset,
            Mode::Dark => ACCENTS[accent as usize].dark.resolve(self.depth),
            Mode::Light => ACCENTS[accent as usize].light.resolve(self.depth),
        }
    }

    /// The app background.
    #[must_use]
    pub const fn background(self) -> Color {
        self.ramp(Tone::Ink0, Tone::Snow)
    }

    /// A surface raised above the background: sidebar, dialog.
    #[must_use]
    pub const fn raised(self) -> Color {
        self.ramp(Tone::Ink1, Tone::Chalk)
    }

    /// The surface a code or tool block sits on, inside the transcript.
    ///
    /// #97's table gives this to `ink-2` on a dark terminal and leaves the
    /// light column empty, so a light terminal reuses [`Theme::raised`]: there
    /// is no step between `snow` and `chalk` to spend on it, and inventing one
    /// would be the second palette this design does not have.
    ///
    /// The role exists now rather than when the transcript slice needs it,
    /// because a ramp step no role can reach is a shade a caller would have to
    /// go around the theme to use — which is the thing the private ramp exists
    /// to prevent.
    #[must_use]
    pub const fn code_surface(self) -> Color {
        self.ramp(Tone::Ink2, Tone::Chalk)
    }

    /// Body text.
    #[must_use]
    pub const fn body(self) -> Color {
        self.ramp(Tone::Chalk, Tone::Steel1)
    }

    /// Secondary body text.
    #[must_use]
    pub const fn secondary(self) -> Color {
        self.ramp(Tone::Mist, Tone::Ash)
    }

    /// Muted text: the status line, timestamps.
    ///
    /// The one role that is the same step in both directions — `smoke` sits at
    /// the middle of the ramp, so it is equally far from either background.
    #[must_use]
    pub const fn muted(self) -> Color {
        self.ramp(Tone::Smoke, Tone::Smoke)
    }

    /// The border of a pane that does not have focus, and separators.
    #[must_use]
    pub const fn border_idle(self) -> Color {
        self.ramp(Tone::Steel0, Tone::Mist)
    }

    /// The border of the focused pane, and the gutter rule.
    ///
    /// #97's table names no active border for a light terminal — it stops at
    /// `mist` for the inactive one — so this takes `ash`, the next step with
    /// enough contrast to read as deliberate without reaching `steel-1`, which
    /// is body text there.
    #[must_use]
    pub const fn border_active(self) -> Color {
        self.ramp(Tone::Steel1, Tone::Ash)
    }

    /// Focus: the focused pane's border, the caret, the selected row, the
    /// active dialog option.
    #[must_use]
    pub const fn focus(self) -> Color {
        self.accent(Accent::Blade)
    }

    /// Added or completed: a diff `+` line, a tool that finished.
    #[must_use]
    pub const fn added(self) -> Color {
        self.accent(Accent::Edge)
    }

    /// Removed or failed: a diff `-` line, a tool that failed, an error.
    #[must_use]
    pub const fn removed(self) -> Color {
        self.accent(Accent::Strike)
    }

    /// Attention: a warning, a permission waiting on the user.
    #[must_use]
    pub const fn warning(self) -> Color {
        self.accent(Accent::Ember)
    }
}

#[cfg(test)]
mod tests {
    use super::{Depth, Glyph, GlyphSet, Mode, Theme, ACCENTS, RAMP, SPINNER};
    use ratatui::style::Color;
    use std::collections::HashMap;

    /// Every role, so a test can sweep them without naming each one twice.
    fn roles(theme: Theme) -> Vec<Color> {
        vec![
            theme.background(),
            theme.raised(),
            theme.code_surface(),
            theme.body(),
            theme.secondary(),
            theme.muted(),
            theme.border_idle(),
            theme.border_active(),
            theme.focus(),
            theme.added(),
            theme.removed(),
            theme.warning(),
        ]
    }

    /// WCAG 2.1 relative luminance, on the truecolor form of a role.
    ///
    /// Only [`Depth::TrueColor`] is checked, and deliberately: the downsampled
    /// forms are a fixed map onto palettes this crate does not define, so a
    /// ratio computed against xterm-256's idea of index 240 would be measuring
    /// that palette rather than this ramp.
    // The coefficients are quoted from WCAG 2.1 as a weighted sum, which is how
    // the specification writes them. `suboptimal_flops` would have this as
    // nested `mul_add` calls; the arithmetic is identical and the resemblance to
    // the published formula is not, so the lint loses here.
    #[allow(clippy::suboptimal_flops)]
    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("contrast is only defined on the truecolor form, got {color:?}")
        };
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    /// The WCAG contrast ratio between two roles, always ≥ 1.0.
    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// A role accessor, so the tables below can be swept rather than unrolled.
    type Role = fn(Theme) -> Color;

    /// The environment one detection case hands to [`Theme::detect`].
    type Vars = &'static [(&'static str, &'static str)];
    /// A row of the mode-precedence table: what it shows, the environment, the answer.
    type ModeCase = (&'static str, Vars, Mode);
    /// A row of the depth-precedence table.
    type DepthCase = (&'static str, Vars, Depth);
    /// A row of the glyph-selection table; the `bool` is `--ascii`.
    type GlyphCase = (&'static str, Vars, bool, GlyphSet);

    /// Everything a foreground role can be drawn on.
    const SURFACES: [(&str, Role); 3] = [
        ("background", Theme::background),
        ("raised", Theme::raised),
        ("code_surface", Theme::code_surface),
    ];

    /// The roles #128's Acceptance puts a floor under, with that floor.
    ///
    /// `border_idle` and `border_active` are **not** here, and that is the
    /// mascot rule rather than an omission: the target is "any border or glyph
    /// **that carries meaning** ≥ 3:1", and in this design a grey carries
    /// structure while colour carries meaning. The border that means something
    /// — the focused pane's — is `focus()`, which is on this list. The grey
    /// borders are separators, and holding them to 3:1 against their own
    /// surface would light up every idle rule on the screen, which is the
    /// opposite of "a screen with nothing happening on it is entirely grey".
    /// They get their own weaker invariant below.
    ///
    /// `muted` is absent for a different reason: Acceptance names body,
    /// secondary, and meaningful border or glyph, and does not say which tier
    /// muted is in. Inventing one would rewrite the ramp — see
    /// `muted_is_more_recessive_than_secondary_but_still_distinct` and the
    /// issue filed against the light theme's raised surface.
    const GATED: [(&str, Role, f64); 6] = [
        ("body", Theme::body, 7.0),
        ("secondary", Theme::secondary, 4.5),
        ("focus", Theme::focus, 3.0),
        ("added", Theme::added, 3.0),
        ("removed", Theme::removed, 3.0),
        ("warning", Theme::warning, 3.0),
    ];

    #[test]
    fn every_permitted_pair_meets_its_contrast_target() {
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode);
            for (surface_name, surface) in SURFACES {
                for (role_name, role, target) in GATED {
                    let ratio = contrast(role(theme), surface(theme));
                    assert!(
                        ratio >= target,
                        "{mode:?}: {role_name} on {surface_name} is {ratio:.2}:1, \
                         below its {target}:1 target — move the ramp value and \
                         update #97's table to match. The target does not move."
                    );
                }
            }
        }
    }

    #[test]
    fn muted_is_more_recessive_than_secondary_but_still_distinct() {
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode);
            for (surface_name, surface) in SURFACES {
                let muted = contrast(theme.muted(), surface(theme));
                let secondary = contrast(theme.secondary(), surface(theme));
                assert!(
                    muted > 1.0,
                    "{mode:?}: muted is invisible on {surface_name}"
                );
                assert!(
                    muted < secondary,
                    "{mode:?}: muted reads at {muted:.2}:1 on {surface_name}, \
                     no quieter than secondary at {secondary:.2}:1 — a role \
                     named muted that is not muted"
                );
            }
        }
    }

    #[test]
    fn the_active_border_reads_stronger_than_the_idle_one() {
        for mode in [Mode::Dark, Mode::Light] {
            let theme = Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode);
            for (surface_name, surface) in SURFACES {
                let idle = contrast(theme.border_idle(), surface(theme));
                let active = contrast(theme.border_active(), surface(theme));
                assert!(
                    active > idle,
                    "{mode:?}: the active border is {active:.2}:1 on \
                     {surface_name} and the idle one {idle:.2}:1 — the pane with \
                     focus would not look any different"
                );
            }
        }
    }

    /// Detect against exactly these variables and nothing else.
    ///
    /// The closure is the injection: no test here can see, or disturb, the
    /// environment the test runner happens to be in.
    fn detect(vars: &[(&str, &str)], force_ascii: bool) -> Theme {
        let env: HashMap<&str, &str> = vars.iter().copied().collect();
        Theme::detect(
            |key| env.get(key).map(|value| (*value).to_owned()),
            force_ascii,
        )
    }

    #[test]
    fn mode_precedence_no_color_then_override_then_colorfgbg_then_dark() {
        let cases: &[ModeCase] = &[
            ("nothing set at all", &[], Mode::Dark),
            (
                "COLORFGBG alone, dark background",
                &[("COLORFGBG", "15;0")],
                Mode::Dark,
            ),
            (
                "COLORFGBG alone, light background",
                &[("COLORFGBG", "0;15")],
                Mode::Light,
            ),
            (
                "COLORFGBG with a middle field",
                &[("COLORFGBG", "15;default;0")],
                Mode::Dark,
            ),
            (
                "COLORFGBG unparseable falls through",
                &[("COLORFGBG", "15;default")],
                Mode::Dark,
            ),
            (
                "the override beats COLORFGBG",
                &[("COLORFGBG", "15;0"), ("JAN_KLOD_THEME", "light")],
                Mode::Light,
            ),
            (
                "the override can also ask for mono",
                &[("JAN_KLOD_THEME", "mono")],
                Mode::Mono,
            ),
            (
                "an unrecognised override is not an instruction",
                &[("JAN_KLOD_THEME", "sepia"), ("COLORFGBG", "0;15")],
                Mode::Light,
            ),
            (
                "NO_COLOR beats the override",
                &[
                    ("NO_COLOR", "1"),
                    ("JAN_KLOD_THEME", "light"),
                    ("COLORFGBG", "0;15"),
                ],
                Mode::Mono,
            ),
            (
                "NO_COLOR counts even when empty",
                &[("NO_COLOR", ""), ("JAN_KLOD_THEME", "dark")],
                Mode::Mono,
            ),
        ];

        for (what, vars, expected) in cases {
            assert_eq!(detect(vars, false).mode(), *expected, "{what}");
        }
    }

    #[test]
    fn depth_precedence_colorterm_then_term_then_sixteen() {
        let cases: &[DepthCase] = &[
            ("nothing set at all", &[], Depth::Basic16),
            ("a plain TERM", &[("TERM", "xterm")], Depth::Basic16),
            (
                "TERM says 256",
                &[("TERM", "xterm-256color")],
                Depth::Indexed256,
            ),
            (
                "COLORTERM says truecolor",
                &[("COLORTERM", "truecolor")],
                Depth::TrueColor,
            ),
            (
                "COLORTERM says 24bit",
                &[("COLORTERM", "24bit")],
                Depth::TrueColor,
            ),
            (
                "COLORTERM beats TERM",
                &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
                Depth::TrueColor,
            ),
            (
                "an unrecognised COLORTERM falls through",
                &[("COLORTERM", "yes"), ("TERM", "xterm-256color")],
                Depth::Indexed256,
            ),
        ];

        for (what, vars, expected) in cases {
            assert_eq!(detect(vars, false).depth(), *expected, "{what}");
        }
    }

    #[test]
    fn glyphs_come_from_the_locale_unless_ascii_is_forced() {
        let cases: &[GlyphCase] = &[
            ("no locale at all", &[], false, GlyphSet::Ascii),
            (
                "LANG names UTF-8",
                &[("LANG", "en_US.UTF-8")],
                false,
                GlyphSet::Unicode,
            ),
            (
                "lower case and no hyphen",
                &[("LANG", "en_US.utf8")],
                false,
                GlyphSet::Unicode,
            ),
            (
                "LC_CTYPE when LANG is unset",
                &[("LC_CTYPE", "C.UTF-8")],
                false,
                GlyphSet::Unicode,
            ),
            (
                "LC_ALL=C beats a UTF-8 LANG",
                &[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")],
                false,
                GlyphSet::Ascii,
            ),
            (
                "LC_CTYPE beats LANG",
                &[("LC_CTYPE", "C"), ("LANG", "en_US.UTF-8")],
                false,
                GlyphSet::Ascii,
            ),
            (
                "a non-UTF-8 locale",
                &[("LANG", "en_US.ISO8859-1")],
                false,
                GlyphSet::Ascii,
            ),
            (
                "--ascii beats the locale",
                &[("LANG", "en_US.UTF-8")],
                true,
                GlyphSet::Ascii,
            ),
        ];

        for (what, vars, force_ascii, expected) in cases {
            assert_eq!(detect(vars, *force_ascii).glyphs(), *expected, "{what}");
        }
    }

    #[test]
    fn detection_reads_nothing_the_caller_did_not_hand_it() {
        // Every variable detection knows about, set to the opposite of the
        // defaults — through the injected map only. If the implementation ever
        // reaches for `std::env` instead, this stops matching.
        let theme = detect(
            &[
                ("JAN_KLOD_THEME", "light"),
                ("COLORTERM", "truecolor"),
                ("LANG", "en_US.UTF-8"),
            ],
            false,
        );
        assert_eq!(theme.mode(), Mode::Light);
        assert_eq!(theme.depth(), Depth::TrueColor);
        assert_eq!(theme.glyphs(), GlyphSet::Unicode);

        let bare = detect(&[], false);
        assert_eq!(bare.mode(), Mode::Dark);
        assert_eq!(bare.depth(), Depth::Basic16);
        assert_eq!(bare.glyphs(), GlyphSet::Ascii);
    }

    #[test]
    fn no_colour_literal_survives_outside_this_module() {
        use std::fs;
        use std::path::Path;

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut checked = 0_usize;

        for dir in ["src", "tests"] {
            let entries = fs::read_dir(root.join(dir)).expect("the crate's own directories exist");
            for entry in entries {
                let path = entry.expect("a readable directory entry").path();
                if path.extension().is_none_or(|ext| ext != "rs")
                    || path.file_name().is_some_and(|name| name == "theme.rs")
                {
                    continue;
                }
                let text = fs::read_to_string(&path).expect("a readable source file");
                checked += 1;
                for (n, line) in text.lines().enumerate() {
                    // Everything from the first `//` is a comment. A `//` inside
                    // a string literal would truncate the line early, which can
                    // only make this check *miss* something, never invent one.
                    let code = line.split("//").next().unwrap_or_default();
                    assert!(
                        !code.contains("Color::"),
                        "{}:{}: a colour literal outside theme.rs — ask the \
                         theme for a role instead, or the next terminal that \
                         cannot render it has no fallback:\n  {}",
                        path.display(),
                        n + 1,
                        line.trim()
                    );
                }
            }
        }

        // A grep that greps nothing passes for the wrong reason. Five sources
        // and three integration tests today, minus theme.rs itself.
        assert!(
            checked >= 7,
            "only {checked} files were scanned — this test has stopped finding \
             the crate it is supposed to guard"
        );
    }

    #[test]
    fn every_glyph_has_an_ascii_form_and_it_differs_where_it_must() {
        for glyph in Glyph::ALL {
            let (unicode, ascii) = glyph.forms();
            assert!(
                !ascii.is_empty(),
                "{glyph:?} has no ASCII form — a terminal without Unicode would \
                 draw nothing where a state should be"
            );
            assert!(!unicode.is_empty(), "{glyph:?} has no Unicode form");
            if !unicode.is_ascii() {
                assert_ne!(
                    unicode, ascii,
                    "{glyph:?} claims an ASCII fallback that is not the fallback \
                     for anything — the Unicode form is not ASCII"
                );
            }
        }
        for frames in [SPINNER.0, SPINNER.1] {
            assert!(!frames.is_empty(), "a spinner with no frames cannot spin");
            assert!(frames.iter().all(|f| !f.is_empty()));
        }
    }

    #[test]
    fn monochrome_still_tells_every_state_apart() {
        // The caret is the composer's cursor rather than a state, and it is the
        // one mark whose position already says what it is; it shares `>` with
        // `Collapsed` in ASCII, which is #97's table as drawn. Every actual
        // state has to stand alone, in both vocabularies, because under
        // `Mode::Mono` the glyph is the entire signal.
        let states = Glyph::ALL.iter().filter(|g| **g != Glyph::Caret);

        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Mono, Depth::TrueColor, set);
            let mut seen: Vec<&str> = Vec::new();
            for state in states.clone() {
                let mark = theme.glyph(*state);
                assert!(
                    !seen.contains(&mark),
                    "{set:?}: {state:?} draws {mark:?}, which another state \
                     already uses — with colour gone there is nothing left to \
                     tell them apart"
                );
                seen.push(mark);
            }
        }
    }

    #[test]
    fn the_vocabulary_does_not_depend_on_colour() {
        for mode in [Mode::Dark, Mode::Light, Mode::Mono] {
            for glyph in Glyph::ALL {
                assert_eq!(
                    Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode).glyph(glyph),
                    glyph.forms().0,
                    "the glyph a state draws must not change with the colour mode"
                );
            }
        }
    }

    #[test]
    fn mono_emits_no_colour_at_all() {
        for depth in [Depth::TrueColor, Depth::Indexed256, Depth::Basic16] {
            for role in roles(Theme::new(Mode::Mono, depth, GlyphSet::Unicode)) {
                assert_eq!(
                    role,
                    Color::Reset,
                    "monochrome must leave the terminal's own colours alone, \
                     even where the terminal could render more"
                );
            }
        }
    }

    #[test]
    fn depth_changes_the_representation_not_the_role() {
        for mode in [Mode::Dark, Mode::Light] {
            assert!(matches!(
                Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode).body(),
                Color::Rgb(..)
            ));
            assert!(matches!(
                Theme::new(mode, Depth::Indexed256, GlyphSet::Unicode).body(),
                Color::Indexed(_)
            ));
            // The sixteen are named variants, so "not Rgb and not Indexed" is
            // the whole claim available here.
            let basic = Theme::new(mode, Depth::Basic16, GlyphSet::Unicode).body();
            assert!(!matches!(basic, Color::Rgb(..) | Color::Indexed(_)));
        }
    }

    #[test]
    fn the_ramp_is_ten_distinct_steps_running_light_to_dark() {
        let mut seen = Vec::new();
        for (i, step) in RAMP.iter().enumerate() {
            assert!(!seen.contains(&step.rgb), "step {i} repeats an earlier one");
            seen.push(step.rgb);
            if i > 0 {
                assert!(
                    step.xterm < RAMP[i - 1].xterm,
                    "step {i} is not darker than the one before it"
                );
            }
        }
        assert_eq!(seen.len(), 10);
    }

    #[test]
    fn dark_and_light_read_the_one_ramp_from_opposite_ends() {
        let dark = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let light = Theme::new(Mode::Light, Depth::TrueColor, GlyphSet::Unicode);

        let darkest = RAMP[RAMP.len() - 1].resolve(Depth::TrueColor);
        let lightest = RAMP[0].resolve(Depth::TrueColor);

        assert_eq!(dark.background(), darkest);
        assert_eq!(light.background(), lightest);
        // …and no second palette: the light theme's text comes out of the same
        // ten swatches the dark one draws its surfaces from.
        assert!(RAMP
            .iter()
            .any(|s| s.resolve(Depth::TrueColor) == light.body()));
    }

    #[test]
    fn every_ramp_step_is_reachable_through_some_role() {
        let reachable: Vec<Color> = [Mode::Dark, Mode::Light]
            .into_iter()
            .flat_map(|mode| roles(Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode)))
            .collect();

        for (i, step) in RAMP.iter().enumerate() {
            assert!(
                reachable.contains(&step.resolve(Depth::TrueColor)),
                "ramp step {i} is not reachable through any role in either mode — \
                 a shade a caller can only get at by going around the theme is \
                 the thing the private ramp exists to prevent"
            );
        }
    }

    #[test]
    fn each_accent_differs_between_the_two_directions() {
        for (i, accent) in ACCENTS.iter().enumerate() {
            assert_ne!(
                accent.dark.rgb, accent.light.rgb,
                "accent {i} is the same colour on a light terminal, which cannot \
                 both meet contrast against ink-0 and against snow"
            );
        }
    }
}
