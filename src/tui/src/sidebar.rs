//! The sidebar (#104): a projection, never a driver.
//!
//! [`view`] takes `&App` plus the connection facts known once at start-up
//! ([`SessionInfo`]) and returns lines to draw — nothing here polls, nothing
//! here sends anything to anywhere. Every value in SESSION, THIS TURN and
//! CHANGED comes from a notification the model has already recorded; the
//! sidebar reads it back rather than asking again.
//!
//! `the_sidebar_only_ever_reads` (in `tests/sidebar_projection.rs`) is the
//! acceptance for that: a grep over this file's own source refusing the two
//! words that would mean it had grown a way to drive anything. That test
//! lives outside this module rather than inside it, so the grep never has to
//! read its own assertion text back and trip on the words it is checking for.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Entry, ToolBlock, ToolStatus, Who};
use crate::blocks;
use crate::diff;
use crate::theme::{Glyph, Theme};

/// Connection facts, fixed for the run.
///
/// Passed in rather than read from the model — the client learned them at
/// start-up (the session id it opened, how it reached the core, where it ran),
/// and the sidebar only ever displays them.
#[derive(Debug, Clone, Copy)]
pub struct SessionInfo<'a> {
    /// The session id.
    pub id: &'a str,
    /// How the client reached the core, already resolved to a string by the
    /// caller — `describe()`'s own words.
    pub via: &'a str,
    /// Where the client was started, for the working-directory row.
    pub cwd: &'a Path,
}

/// The sidebar's three sections, as lines ready to draw at `width` cells.
#[must_use]
pub fn view(app: &App, session: SessionInfo<'_>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    heading(&mut lines, "SESSION", width, theme);
    row(&mut lines, format!("id    {}", session.id), width, theme);
    row(&mut lines, format!("via   {}", session.via), width, theme);
    row(
        &mut lines,
        format!("state {}", app.connection_state()),
        width,
        theme,
    );
    let cwd = session.cwd.display().to_string();
    let budget = width.saturating_sub("cwd   ".len());
    row(
        &mut lines,
        format!("cwd   {}", truncate_from_left(theme, &cwd, budget)),
        width,
        theme,
    );

    lines.push(Line::default());
    heading(&mut lines, "THIS TURN", width, theme);
    let turn_tools = this_turn_tools(app);
    if turn_tools.is_empty() {
        row(&mut lines, "(nothing yet)".to_string(), width, theme);
    } else {
        for tool in turn_tools {
            row(
                &mut lines,
                format!("{} {}", tool_glyph(tool, theme), tool.name),
                width,
                theme,
            );
        }
    }

    // Only when something contributes: an empty section would teach a reader
    // to expect one, and every session without extensions would carry a row
    // saying so. `THIS TURN` shows "(nothing yet)" because a turn always
    // exists; an extension that contributes nothing has nothing to report.
    let contributed = app.contributed_status();
    if !contributed.is_empty() {
        lines.push(Line::default());
        heading(&mut lines, "EXTENSIONS", width, theme);
        for item in contributed {
            // Cut to the pane here, where the cell is known. The text was
            // already made inert when it arrived; this only fits it.
            row(
                &mut lines,
                // `row` cuts and marks this like every other line now (#206).
                format!("{} {}", item.extension, item.text),
                width,
                theme,
            );
        }
    }

    lines.push(Line::default());
    heading(&mut lines, "CHANGED", width, theme);
    let changed = changed_files(app);
    if changed.is_empty() {
        row(&mut lines, "(nothing yet)".to_string(), width, theme);
    } else {
        for (path, counts) in changed {
            let text = counts.map_or_else(
                || path.clone(),
                |(added, removed)| format!("{path} +{added} -{removed}"),
            );
            row(&mut lines, text, width, theme);
        }
    }

    lines
}

fn heading(lines: &mut Vec<Line<'static>>, text: &str, width: usize, theme: Theme) {
    lines.push(Line::from(Span::styled(
        cut(text, width, theme),
        Style::default().fg(theme.muted()),
    )));
}

fn row(lines: &mut Vec<Line<'static>>, text: String, width: usize, theme: Theme) {
    lines.push(Line::from(Span::styled(
        cut(&text, width, theme),
        Style::default().fg(theme.secondary()),
    )));
}

/// Every line this widget emits, cut to the pane and marked when cut.
///
/// **In one place on purpose.** Before this, `cwd` cut itself and every other
/// row was handed to `ratatui` whole and clipped at the frame with no marker,
/// so a shortened value read as the whole value (#206). Putting the cut in
/// the two functions every line goes through fixes the rows that exist and
/// the rows a later section adds — which is how this arrived: the EXTENSIONS
/// section came with #190 and brought the bug with it.
///
/// `cwd` is unaffected: it pre-cuts from the left into `width - "cwd   "`, so
/// the finished line is already exactly `width` and this is a no-op on it.
/// A path's leaf is the readable part, which is why that one cut runs the
/// other way.
fn cut(text: &str, width: usize, theme: Theme) -> String {
    crate::untrusted::inert_within(text, width, theme.glyph(Glyph::Elided))
}

/// Truncated from left: the readable part (the leaf) stays visible, not the
/// root of a long path.
fn truncate_from_left(theme: Theme, text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if crate::wrap::width(text) <= max {
        return text.to_string();
    }
    let mark = theme.glyph(Glyph::Elided);
    let budget = max.saturating_sub(crate::wrap::width(mark));
    let mut kept = String::new();
    for ch in text.chars().rev() {
        let candidate = format!("{ch}{kept}");
        if crate::wrap::width(&candidate) > budget {
            break;
        }
        kept = candidate;
    }
    format!("{mark}{kept}")
}

/// The tool calls belonging to the turn in progress — every [`Entry::Tool`]
/// after the most recent [`Who::You`] line, in call order. Cleared when
/// a new turn's message is recorded, since counting resumes after it.
fn this_turn_tools(app: &App) -> Vec<&ToolBlock> {
    let start = app
        .transcript
        .iter()
        .rposition(|entry| matches!(entry, Entry::Message { who: Who::You, .. }))
        .map_or(0, |i| i + 1);
    app.transcript[start..]
        .iter()
        .filter_map(|entry| match entry {
            Entry::Tool(tool) => Some(tool),
            Entry::Message { .. } => None,
        })
        .collect()
}

fn tool_glyph(tool: &ToolBlock, theme: Theme) -> &'static str {
    match &tool.status {
        ToolStatus::Running => theme.spinner().first().copied().unwrap_or("-"),
        ToolStatus::Done { failed: true, .. } | ToolStatus::Interrupted => {
            theme.glyph(Glyph::ToolFailed)
        }
        ToolStatus::Done { failed: false, .. } => theme.glyph(Glyph::ToolDone),
    }
}

/// Files this session's tool calls touched, oldest first, deduplicated by
/// path (a later call against the same path replaces its counts rather than
/// adding a second row).
///
/// `None` where [`diff::counts`] could not parse the result as a diff — the
/// row then shows the path alone. **Never `+0 -0`**: that would be a claim
/// this client cannot back up, since a result that is not a diff is not
/// evidence that nothing changed.
///
/// Reads [`ToolBlock`] directly rather than a second, redrawn "changed files"
/// field on [`App`] — #104 asks for CHANGED to come from "the same model, not
/// recomputed", and a duplicate list is exactly the second source of truth
/// that would stop meaning that.
fn changed_files(app: &App) -> Vec<(String, Option<(usize, usize)>)> {
    let mut ordered: Vec<(String, Option<(usize, usize)>)> = Vec::new();
    for entry in &app.transcript {
        let Entry::Tool(tool) = entry else { continue };
        let ToolStatus::Done {
            content,
            failed: false,
        } = &tool.status
        else {
            continue;
        };
        let path = blocks::summary(tool);
        if path.is_empty() {
            continue;
        }
        let counts = diff::counts(content);
        if let Some(existing) = ordered.iter_mut().find(|(seen, _)| *seen == path) {
            existing.1 = counts;
        } else {
            ordered.push((path, counts));
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::{view, SessionInfo};
    use crate::app::App;
    use crate::theme::{Depth, GlyphSet, Mode, Theme};

    fn theme() -> Theme {
        Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode)
    }

    fn plain(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn info(cwd: &std::path::Path) -> SessionInfo<'_> {
        SessionInfo {
            id: "sess-1",
            via: "jan-klod-gateway over stdio",
            cwd,
        }
    }

    /// One extension reporting one status item, as the notification delivers it.
    fn reporting(text: &str) -> Vec<jan_klod_protocol::Contributions> {
        vec![jan_klod_protocol::Contributions {
            extension: "interceptor.system".to_owned(),
            commands: vec![],
            status_items: vec![jan_klod_protocol::StatusItem {
                name: "prompt-source".to_owned(),
                text: text.to_owned(),
                detail: "why".to_owned(),
            }],
            forms: vec![],
        }]
    }

    #[test]
    fn a_contributed_status_item_gets_a_section_naming_who_said_it() {
        let mut app = App::default();
        app.set_contributions(&reporting("built-in"));
        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 40, theme()));
        assert!(text.contains("EXTENSIONS"), "{text}");
        assert!(
            text.contains("interceptor.system built-in"),
            "the row says who is claiming it: {text}"
        );
    }

    /// An empty section would teach a reader to expect one, so every session
    /// without extensions would carry a row saying there are none.
    #[test]
    fn no_contributions_means_no_section_at_all() {
        let app = App::default();
        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 40, theme()));
        assert!(!text.contains("EXTENSIONS"), "{text}");
    }

    /// The second half of the escaping rule: cleaned at the door, and still
    /// inert after a frame has laid it out.
    #[test]
    fn a_hostile_status_item_is_inert_in_the_rendered_line() {
        let mut app = App::default();
        app.set_contributions(&reporting("safe\x1b[2J"));
        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 40, theme()));
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    /// A long claim cannot push the pane wider; it is cut to the cell, and
    /// says it was cut rather than reading as a shorter claim.
    ///
    /// Only the contributed rows are checked. The fixed rows above them —
    /// `id`, `via` — are the client's own words and are left to the frame's
    /// clipping, which is a pre-existing choice this slice does not change.
    #[test]
    fn a_long_status_item_is_cut_to_the_pane() {
        let mut app = App::default();
        app.set_contributions(&reporting(&"wide".repeat(40)));
        let cwd = std::path::PathBuf::from("/repo");
        let rows = view(&app, info(&cwd), 20, theme());
        let contributed = rows
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("interceptor.system"))
            })
            .expect("the contributed row is drawn");
        let width: usize = contributed
            .spans
            .iter()
            .map(|span| crate::wrap::width(span.content.as_ref()))
            .sum();
        assert_eq!(width, 20, "it fills exactly the pane");
        assert!(
            contributed
                .spans
                .iter()
                .any(|span| span.content.ends_with('…')),
            "a cut row has to say it was cut"
        );
    }

    /// Acceptance: CHANGED shows a path with no counts when the result could
    /// not be parsed as a diff — never `+0 -0`, which would be a claim this
    /// client cannot back up.
    #[test]
    fn changed_shows_a_bare_path_rather_than_guessing_zero_counts() {
        let mut app = App::default();
        app.record_tool_invoked(
            "c1".into(),
            "edit".into(),
            Some(r#"{"path":"src/main.rs"}"#.into()),
        );
        app.record_tool_result(
            "c1",
            "replace applied to src/main.rs (1 line(s)).".into(),
            false,
        );

        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 28, theme()));
        assert!(text.contains("src/main.rs"), "{text:?}");
        assert!(!text.contains("+0 -0"), "{text:?}");
        assert!(!text.contains('+'), "a count was invented: {text:?}");
    }

    /// A parsed diff's counts do show, and by the same path `blocks` uses.
    #[test]
    fn changed_shows_real_counts_when_the_result_parses_as_a_diff() {
        let mut app = App::default();
        app.record_tool_invoked(
            "c1".into(),
            "git".into(),
            Some(r#"{"path":"src/main.rs"}"#.into()),
        );
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
             --- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context";
        app.record_tool_result("c1", diff.into(), false);

        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 28, theme()));
        assert!(text.contains("src/main.rs +1 -1"), "{text:?}");
    }

    /// THIS TURN clears at the next user message, and shows call order.
    #[test]
    fn this_turn_is_cleared_by_the_next_message() {
        let mut app = App::default();
        "first".chars().for_each(|c| app.push_char(c));
        app.take_submission();
        app.record_tool_invoked("a".into(), "read".into(), None);
        app.record_tool_invoked("b".into(), "write".into(), None);

        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 28, theme()));
        assert!(text.contains("read"), "{text:?}");
        assert!(text.contains("write"), "{text:?}");
        let read_at = text.find("read").unwrap();
        let write_at = text.find("write").unwrap();
        assert!(read_at < write_at, "not in call order: {text:?}");

        "second".chars().for_each(|c| app.push_char(c));
        app.take_submission();
        let cleared = plain(&view(&app, info(&cwd), 28, theme()));
        // `state ready` contains the substring "read", so this checks the
        // THIS TURN section on its own rather than the whole rendering.
        let this_turn = cleared
            .split("THIS TURN")
            .nth(1)
            .and_then(|rest| rest.split("CHANGED").next())
            .unwrap_or_default();
        assert!(
            this_turn.contains("nothing yet"),
            "a new turn's message did not clear the old one: {this_turn:?}"
        );
    }

    /// A long path is truncated from the left, so the leaf stays readable.
    #[test]
    fn the_working_directory_is_truncated_from_the_left() {
        let app = App::default();
        let cwd = std::path::PathBuf::from("/very/deeply/nested/workspace/leaf-dir");
        let text = plain(&view(&app, info(&cwd), 28, theme()));
        assert!(text.contains("leaf-dir"), "the leaf was cut: {text:?}");
        assert!(
            !text.contains("/very/deeply"),
            "the root should have been the part cut: {text:?}"
        );
    }
}
