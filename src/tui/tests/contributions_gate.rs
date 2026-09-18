#![allow(missing_docs)]
//! #190's acceptance: at 120×32 and 60×20, with `NO_COLOR=1`, contributions
//! render, are invocable, and lose no information — and a hostile label
//! renders inert.
//!
//! Shaped like `phase19_gate.rs`, for the same reason it exists: an exit
//! criterion written as prose is a criterion nobody runs (#130, #133).
//!
//! # Why a hand-built contribution rather than a real guest
//!
//! `host/tests/it/client_surface.rs` already drives a real component's
//! declarations through the host and out to a client. What is under test here
//! is the **renderer** — at two terminal sizes, in two themes, against text an
//! honest extension would never send. A wasm build in the way of that would
//! add no coverage and a great deal of setup.

use jan_klod::app::App;
use jan_klod::layout;
use jan_klod::sidebar::{self, SessionInfo};
use jan_klod::theme::{Depth, GlyphSet, Mode, Theme};
use jan_klod::wrap::width as cells;
use jan_klod_protocol::{Contributions, StatusItem, SurfaceCommand};

/// The gate's themes: coloured, and what `NO_COLOR=1` on 16 colours gives.
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

/// One extension contributing a command and a status item, as the host
/// reports them.
fn contributed(name: &str, status: &str) -> Vec<Contributions> {
    vec![Contributions {
        extension: "interceptor.system".to_owned(),
        commands: vec![SurfaceCommand {
            name: name.to_owned(),
            title: "System prompt".to_owned(),
            description: "show the standing instructions".to_owned(),
            arguments: vec![],
        }],
        status_items: vec![StatusItem {
            name: "prompt-source".to_owned(),
            text: status.to_owned(),
            detail: "no `prompt` key is configured".to_owned(),
        }],
        forms: vec![],
    }]
}

fn plain(lines: &[ratatui::text::Line<'_>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An app with the contribution loaded and the command menu open.
fn with_menu_open(sets: &[Contributions]) -> App {
    let mut app = App::default();
    app.set_contributions(sets);
    app.push_char('/');
    app
}

#[test]
fn contributions_render_at_both_sizes_in_both_themes() {
    let app = with_menu_open(&contributed("prompt", "built-in"));
    let cwd = std::path::PathBuf::from("/repo");
    let info = SessionInfo {
        id: "gate",
        via: "jan-klod-gateway over stdio",
        cwd: &cwd,
    };

    for (w, h) in [(120u16, 32u16), (60, 20)] {
        let frame = layout::regions(w, h);
        assert!(!frame.too_small, "{w}×{h} reports too small");

        assert!(
            app.menu_entries()
                .iter()
                .any(|entry| entry.name == "prompt"),
            "{w}×{h}: the contributed command is not in the menu"
        );

        for (name, theme) in themes() {
            // The sidebar pane is the one with a width of its own; at 60×20 it
            // may be collapsed, in which case the dialog draws the same view.
            let pane = usize::from(frame.sidebar.map_or(frame.content.width, |s| s.width))
                .saturating_sub(2);
            let text = plain(&sidebar::view(&app, info, pane, theme));
            assert!(
                text.contains("EXTENSIONS"),
                "{name} at {w}×{h}: no EXTENSIONS section"
            );
            assert!(
                text.contains("interceptor.system"),
                "{name} at {w}×{h}: the row does not name who claimed it"
            );
            for line in sidebar::view(&app, info, pane, theme) {
                let row: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                if row.contains("interceptor.system") {
                    assert!(
                        cells(&row) <= pane,
                        "{name} at {w}×{h}: a contributed row is {} cells in a {pane}-cell \
                         pane: {row:?}",
                        cells(&row)
                    );
                }
            }
        }
    }
}

/// "Loses no information": what colour carries is also in text, so the two
/// themes render the same characters. The same property `phase19_gate.rs`
/// asserts for a turn, asserted for contributions.
#[test]
fn colour_carries_nothing_a_colourless_render_loses() {
    let app = with_menu_open(&contributed("prompt", "built-in"));
    let cwd = std::path::PathBuf::from("/repo");
    let info = SessionInfo {
        id: "gate",
        via: "jan-klod-gateway over stdio",
        cwd: &cwd,
    };

    let [(_, coloured), (_, mono)] = themes();
    assert_eq!(
        plain(&sidebar::view(&app, info, 38, coloured)),
        plain(&sidebar::view(&app, info, 38, mono)),
        "the colourless render must carry the same text"
    );
}

/// Invocable: choosing the entry raises the ask the event loop sends. The
/// transport is not exercised here — `host/tests/it/client_surface.rs` does
/// that end to end; this is the client half.
#[test]
fn a_contributed_command_can_be_chosen_from_the_menu() {
    let mut app = with_menu_open(&contributed("prompt", "built-in"));
    for c in "prompt".chars() {
        app.push_char(c);
    }
    assert!(app.menu_accept(), "the menu accepted the contributed entry");
    assert_eq!(
        app.take_invoke_request(),
        Some(("interceptor.system".to_owned(), "prompt".to_owned()))
    );
}

/// The hostile case, through the real projection rather than the sanitiser's
/// own unit tests: those prove the function strips, this proves nothing
/// downstream puts it back.
#[test]
fn a_hostile_label_renders_inert_at_both_sizes() {
    let hostile = "pr\x1b[2Jompt";
    let status = "built\x1b]0;pwned\x07-in\u{202e}reversed";
    let app = with_menu_open(&contributed(hostile, status));
    let cwd = std::path::PathBuf::from("/repo");
    let info = SessionInfo {
        id: "gate",
        via: "jan-klod-gateway over stdio",
        cwd: &cwd,
    };

    for (w, h) in [(120u16, 32u16), (60, 20)] {
        let frame = layout::regions(w, h);
        let pane =
            usize::from(frame.sidebar.map_or(frame.content.width, |s| s.width)).saturating_sub(2);
        for (name, theme) in themes() {
            let rendered = plain(&sidebar::view(&app, info, pane, theme));
            for hostile_char in ['\u{1b}', '\u{7}', '\u{202e}', '\u{9b}'] {
                assert!(
                    !rendered.contains(hostile_char),
                    "{name} at {w}×{h}: {hostile_char:?} reached the frame: {rendered:?}"
                );
            }
        }
        // And the menu, which draws from the same store.
        for entry in app.menu_entries() {
            assert!(
                !entry.name.contains('\u{1b}') && !entry.summary.contains('\u{1b}'),
                "{w}×{h}: an escape reached a menu entry: {entry:?}"
            );
        }
    }
}
