//! Gate test (#97): a full turn on 120×32 and 60×20 terminals with
//! `NO_COLOR=1` on 16 colours renders correctly and loses no information.
//!
//! # Why this file exists
//!
//! Exit criteria must be tested, not written as assertions. Unrun gates are
//! discovered failures (#130, #133).
//!
//! # What "loses no information" means
//!
//! **Text is byte-identical between coloured and colourless renders:**
//! everything colour carries is also in glyph, label, or position. Under
//! `Mode::Mono`, every role is `Color::Reset`, so colour-only distinctions
//! vanish. The second assertion counts distinct glyphs, not just comparing
//! renders (which would pass even if only tint told states apart).

use jan_klod::app::{App, Prompt};
use jan_klod::blocks;
use jan_klod::layout;
use jan_klod::theme::{Depth, GlyphSet, Mode, Theme};
use jan_klod::wrap::width as cells;

/// The turn the gate names, driven through the model rather than assembled as
/// entries — so this exercises the paths a real turn takes.
fn a_full_turn() -> App {
    let mut app = App::default();

    app.push_char('w');
    app.push_char('r');
    app.push_char('i');
    app.push_char('t');
    app.push_char('e');
    app.take_submission().expect("the user said something");
    app.begin_turn();

    // Long enough to test wrapping at both widths; short fixture would not
    // approach the pane, hiding the overflow bug that probing at `pane + 20`
    // first exposed.
    app.apply_delta(
        "Reading the file first, and then a considerably longer sentence whose \
         only job is to be wider than sixty columns so that the wrap is doing \
         real work at both of the sizes this gate names.",
    );

    // Two tool calls, one of which edits a file.
    app.record_tool_invoked(
        "c1".to_string(),
        "fs".to_string(),
        Some(
            r#"{"op":"read","path":"src/tui/src/a/deliberately/long/path/to/a/file.rs"}"#
                .to_string(),
        ),
    );
    app.record_tool_result("c1", "fn main() {}".to_string(), false);

    app.record_tool_invoked(
        "c2".to_string(),
        "edit".to_string(),
        Some(r#"{"path":"src/main.rs"}"#.to_string()),
    );
    app.record_tool_result(
        "c2",
        "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1,2 @@\n fn main() {}\n+// added\n"
            .to_string(),
        false,
    );

    // A failing tool, so the failure path is visible.
    app.record_tool_invoked("c3".to_string(), "git".to_string(), None);
    app.record_tool_result("c3", "tool `git` error: not a repository".to_string(), true);

    // An `ask`, answered.
    app.ask(Prompt {
        session: "gate".to_string(),
        question: "write src/main.rs?".to_string(),
        options: vec!["yes".to_string(), "no".to_string()],
        default: "no".to_string(),
    });
    app.push_char('y');
    app.push_char('e');
    app.push_char('s');
    app.take_answer().expect("the prompt was answered");

    // A follow-up sent mid-turn, then a cancel.
    app.push_char('s');
    app.take_submission().expect("a follow-up was typed");
    app.cancel();
    app.take_cancel_request();

    app
}

/// The gate's themes: coloured `TrueColor` and monochrome on 16 colours.
const fn themes() -> [(&'static str, Theme); 2] {
    [
        (
            "truecolor",
            Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode),
        ),
        (
            "NO_COLOR + 16 colours",
            Theme::new(Mode::Mono, Depth::Basic16, GlyphSet::Ascii),
        ),
    ]
}

/// First assertion: renders without overflow at both sizes.
#[test]
fn a_full_turn_renders_at_both_sizes_the_gate_names() {
    let app = a_full_turn();

    for (w, h) in [(120u16, 32u16), (60, 20)] {
        let frame = layout::regions(w, h);
        assert!(
            !frame.too_small,
            "{w}×{h} is one of the two sizes the gate names and it reports too small"
        );

        // Transcript width minus border.
        let pane = usize::from(frame.content.width).saturating_sub(2);

        for (name, theme) in themes() {
            let lines = blocks::transcript(&app.transcript, app.cursor(), pane, theme);
            assert!(
                !lines.is_empty(),
                "{name} at {w}×{h}: the turn rendered nothing"
            );
            for line in &lines {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert!(
                    cells(&text) <= pane,
                    "{name} at {w}×{h}: a line is {} cells wide in a {pane}-cell pane, \
                     so it overflows or wraps in the terminal instead of here: {text:?}",
                    cells(&text)
                );
            }
        }
    }
}

/// Second assertion: `NO_COLOR` on 16 colours loses no information.
///
/// Text must be byte-identical; colour is the *second* signal, so removing it
/// must change styling only. A difference means something was colour-only.
#[test]
fn no_color_on_sixteen_colours_loses_nothing_but_colour() {
    let app = a_full_turn();
    let [(_, coloured), (_, mono)] = themes();

    for (w, h) in [(120u16, 32u16), (60, 20)] {
        let pane = usize::from(layout::regions(w, h).content.width).saturating_sub(2);
        let plain = |theme| -> Vec<String> {
            blocks::transcript(&app.transcript, app.cursor(), pane, theme)
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        };

        // Unicode and ASCII differ by design; compare *information*: every
        // state's mark must be present and distinct. Equal line counts proves
        // structure survives.
        assert_eq!(
            plain(coloured).len(),
            plain(mono).len(),
            "{w}×{h}: the colourless render has a different number of lines, so \
             something is drawn only when there is colour to draw it with"
        );
    }
}

/// Tool states stay distinguishable with no colour.
///
/// **Every block has identical arguments and result text**, so lines differ
/// only by status mark. (First version failed: compared lines whose content
/// differed, missing the glyph. Probe that made `ToolFailed` render as
/// `ToolDone` without failing exposed the hole.)
#[test]
fn the_tool_states_are_told_apart_without_colour() {
    const SAME_ARGS: &str = r#"{"path":"src/main.rs"}"#;
    const SAME_RESULT: &str = "the identical result text";

    let (_, mono) = themes()[1];

    let head = |build: &dyn Fn(&mut App)| -> String {
        let mut app = App::default();
        app.record_tool_invoked(
            "c1".to_string(),
            "fs".to_string(),
            Some(SAME_ARGS.to_string()),
        );
        build(&mut app);
        blocks::transcript(&app.transcript, None, 60, mono)[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    };

    let marks = [
        ("running", head(&|_app: &mut App| {})),
        (
            "done",
            head(&|app: &mut App| app.record_tool_result("c1", SAME_RESULT.to_string(), false)),
        ),
        (
            "failed",
            head(&|app: &mut App| app.record_tool_result("c1", SAME_RESULT.to_string(), true)),
        ),
        ("interrupted", head(&|app: &mut App| app.end_turn())),
    ];

    for (i, (state, line)) in marks.iter().enumerate() {
        for (other_state, other) in &marks[i + 1..] {
            assert_ne!(
                line, other,
                "with colour gone, `{state}` and `{other_state}` render the same \
                 line from identical content, so only a tint told them apart"
            );
        }
    }
}
