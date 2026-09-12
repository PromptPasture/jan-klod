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
//! # What this module does not do
//!
//! It does not decide *which* [`Mode`] or [`Depth`] a terminal has. That is
//! capability detection, which reads the environment, and it is deliberately a
//! separate concern — [`Theme::new`] takes both, so every test can state the
//! terminal it means instead of racing a process-global.

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
const RAMP: [Swatch; 10] = [
    sw((0xF2, 0xF3, 0xF5), 255, Color::White),    // snow
    sw((0xD6, 0xDA, 0xE0), 252, Color::White),    // chalk
    sw((0xB0, 0xB6, 0xBF), 249, Color::Gray),     // mist
    sw((0x86, 0x8C, 0x96), 245, Color::Gray),     // smoke
    sw((0x64, 0x6A, 0x74), 242, Color::DarkGray), // ash
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

/// The design system, resolved for one terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    mode: Mode,
    depth: Depth,
}

impl Theme {
    /// A theme for a terminal whose mode and colour depth are already known.
    ///
    /// Both are arguments rather than something this constructor sniffs, so a
    /// test can name the terminal it means.
    #[must_use]
    pub const fn new(mode: Mode, depth: Depth) -> Self {
        Self { mode, depth }
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
    use super::{Depth, Mode, Theme, ACCENTS, RAMP};
    use ratatui::style::Color;

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

    #[test]
    fn mono_emits_no_colour_at_all() {
        for depth in [Depth::TrueColor, Depth::Indexed256, Depth::Basic16] {
            for role in roles(Theme::new(Mode::Mono, depth)) {
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
                Theme::new(mode, Depth::TrueColor).body(),
                Color::Rgb(..)
            ));
            assert!(matches!(
                Theme::new(mode, Depth::Indexed256).body(),
                Color::Indexed(_)
            ));
            // The sixteen are named variants, so "not Rgb and not Indexed" is
            // the whole claim available here.
            let basic = Theme::new(mode, Depth::Basic16).body();
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
        let dark = Theme::new(Mode::Dark, Depth::TrueColor);
        let light = Theme::new(Mode::Light, Depth::TrueColor);

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
            .flat_map(|mode| roles(Theme::new(mode, Depth::TrueColor)))
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
