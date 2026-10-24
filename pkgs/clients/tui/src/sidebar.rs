//! The sidebar: a projection, never a driver.

use std::path::Path;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{App, Entry, ToolBlock, ToolStatus, Who};
use crate::blocks;
use crate::diff;
use crate::theme::{Glyph, Theme};

/// Connection facts fixed at start-up.
#[derive(Debug, Clone, Copy)]
pub struct SessionInfo<'a> {
    /// The session id.
    pub id: &'a str,
    /// How the client reached the core, in `describe()`'s words.
    pub via: &'a str,
    /// Where the client was started, for the working-directory row.
    pub cwd: &'a Path,
}

/// The sidebar's sections as lines to draw at `width` cells.
#[must_use]
pub fn view(app: &App, session: SessionInfo<'_>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    heading(&mut lines, "SESSION", width, theme);
    row(&mut lines, &format!("id    {}", session.id), width, theme);
    row(&mut lines, &format!("via   {}", session.via), width, theme);
    row(
        &mut lines,
        &format!("state {}", app.connection_state()),
        width,
        theme,
    );
    let cwd = session.cwd.display().to_string();
    let budget = width.saturating_sub("cwd   ".len());
    row(
        &mut lines,
        &format!("cwd   {}", truncate_from_left(theme, &cwd, budget)),
        width,
        theme,
    );

    lines.push(Line::default());
    heading(&mut lines, "THIS TURN", width, theme);
    let turn_tools = this_turn_tools(app);
    if turn_tools.is_empty() {
        row(&mut lines, "(nothing yet)", width, theme);
    } else {
        for tool in turn_tools {
            row(
                &mut lines,
                &format!("{} {}", tool_glyph(tool, theme), tool.name),
                width,
                theme,
            );
        }
    }

    let contributed = app.contributed_status();
    if !contributed.is_empty() {
        lines.push(Line::default());
        heading(&mut lines, "EXTENSIONS", width, theme);
        for item in contributed {
            row(
                &mut lines,
                &format!("{} {}", item.extension, item.text),
                width,
                theme,
            );
        }
    }

    lines.push(Line::default());
    heading(&mut lines, "CHANGED", width, theme);
    let changed = changed_files(app);
    if changed.is_empty() {
        row(&mut lines, "(nothing yet)", width, theme);
    } else {
        for (path, counts) in changed {
            let text = counts.map_or_else(
                || path.clone(),
                |(added, removed)| format!("{path} +{added} -{removed}"),
            );
            row(&mut lines, &text, width, theme);
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

fn row(lines: &mut Vec<Line<'static>>, text: &str, width: usize, theme: Theme) {
    lines.push(Line::from(Span::styled(
        cut(text, width, theme),
        Style::default().fg(theme.secondary()),
    )));
}

fn cut(text: &str, width: usize, theme: Theme) -> String {
    crate::untrusted::inert_within(text, width, theme.glyph(Glyph::Elided))
}

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

/// The tool calls after the latest [`Who::You`] message, in call order.
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

/// Counts are `None` when the result is not a diff.
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

    fn reporting(text: &str) -> Vec<jk_protocol::Contributions> {
        vec![jk_protocol::Contributions {
            extension: "interceptor.system".to_owned(),
            commands: vec![],
            status_items: vec![jk_protocol::StatusItem {
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

    #[test]
    fn no_contributions_means_no_section_at_all() {
        let app = App::default();
        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 40, theme()));
        assert!(!text.contains("EXTENSIONS"), "{text}");
    }

    #[test]
    fn a_hostile_status_item_is_inert_in_the_rendered_line() {
        let mut app = App::default();
        app.set_contributions(&reporting("safe\x1b[2J"));
        let cwd = std::path::PathBuf::from("/repo");
        let text = plain(&view(&app, info(&cwd), 40, theme()));
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

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

    #[test]
    fn every_line_fits_the_pane_it_was_given() {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Dark, Depth::TrueColor, glyphs);
            let mut app = App::default();
            app.set_contributions(&reporting(&"wide".repeat(40)));
            app.record_tool_invoked(
                "c1".into(),
                "a-tool-with-a-very-long-name-indeed".into(),
                Some(r#"{"path":"src/some/deeply/nested/module/file.rs"}"#.into()),
            );
            app.record_tool_result(
                "c1",
                "replace applied to src/some/deeply/nested/module/file.rs (1 line(s)).".into(),
                false,
            );
            let cwd = std::path::PathBuf::from("/a/deep/path/that/will/not/fit/in/the/pane");

            for width in [20, 12, 6] {
                for line in view(&app, info(&cwd), width, theme) {
                    let cells: usize = line
                        .spans
                        .iter()
                        .map(|span| crate::wrap::width(span.content.as_ref()))
                        .sum();
                    assert!(
                        cells <= width,
                        "a {cells}-cell line overflows a {width}-cell pane \
                         with {glyphs:?} glyphs: {:?}",
                        plain(&[line])
                    );
                }
            }
        }
    }

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
