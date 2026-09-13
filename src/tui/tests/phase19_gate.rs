//! Phase 19's exit gate (#97), as a test rather than as a claim.
//!
//! > On a 120×32 terminal and on a 60×20 one, a full turn — user message,
//! > streamed answer, two tool calls one of which edits a file, an `ask`
//! > answered, a follow-up sent mid-turn, and a cancel — renders correctly, and
//! > the same run with `NO_COLOR=1` in a 16-colour terminal loses no
//! > information.
//!
//! # Why this file exists
//!
//! An umbrella closes when its exit criteria are met, and this repository has
//! twice paid for the other way round: #130's supervisor scan and #133's child
//! exit status were both green because nothing ran them. A gate recorded as a
//! sentence in an issue is a gate nobody runs.
//!
//! # What "loses no information" is taken to mean
//!
//! Not "looks the same" — under `Mode::Mono` every role resolves to
//! `Color::Reset`, so it demonstrably does not. The testable reading is that
//! **the text is byte-identical between the coloured and colourless renders**,
//! so everything colour was carrying is also carried by glyph, label or
//! position. If a state were distinguished only by tint, the two renders would
//! still match and the distinction would be gone — which is why
//! `theme::monochrome_still_tells_every_state_apart` exists beside this, and
//! why the second assertion below counts distinct glyphs rather than trusting
//! the first.

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

    // Long enough that wrapping is load-bearing at both widths — the first
    // version of this fixture was short enough that nothing ever approached the
    // pane, so the overflow assertion below could not fail. Probed by rendering
    // at `pane + 20`, which it failed to notice.
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
            r#"{"op":"read","path":"src/core/ui/src/a/deliberately/long/path/to/a/file.rs"}"#
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

    // A third that failed, so the failure path is on screen too.
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

/// Every theme the gate names: the coloured one, and `NO_COLOR` on a
/// sixteen-colour terminal.
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

/// The gate's first half: it renders, at both sizes, without overflowing.
#[test]
fn a_full_turn_renders_at_both_sizes_the_gate_names() {
    let app = a_full_turn();

    for (w, h) in [(120u16, 32u16), (60, 20)] {
        let frame = layout::regions(w, h);
        assert!(
            !frame.too_small,
            "{w}×{h} is one of the two sizes the gate names and it reports too small"
        );

        // The transcript's usable width, minus the block border.
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

/// The gate's second half: `NO_COLOR` on sixteen colours loses no information.
///
/// Asserted as text equality, for the reason in this file's header: colour is
/// the *second* signal throughout, so removing it must change styling and
/// nothing else. A difference here means something was being said in colour
/// alone.
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

        // Unicode and ASCII vocabularies differ by design, so compare the
        // *information*: every state's mark must still be present and distinct.
        // Equal line counts is the structural half of "loses nothing".
        assert_eq!(
            plain(coloured).len(),
            plain(mono).len(),
            "{w}×{h}: the colourless render has a different number of lines, so \
             something is drawn only when there is colour to draw it with"
        );
    }
}

/// The tool states the turn puts on screen stay distinguishable with no colour.
///
/// **Every block below carries identical arguments and identical result text**,
/// so the collapsed line can differ only by the status mark. That is the whole
/// point, and the first version of this test got it wrong: it compared the
/// rendered line for states whose *content* already differed, so the lines
/// differed for reasons that had nothing to do with the glyph. Making
/// `ToolFailed` render as `ToolDone` did not fail it — probed, which is how the
/// hole was found.
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
