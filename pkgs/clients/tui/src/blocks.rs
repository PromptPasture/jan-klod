//! Transcript and pane width to rendered lines, with no drawing.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Entry, Prompt, SessionEntry, ToolBlock, ToolStatus, Who};
use crate::keymap;
use crate::theme::{Glyph, Theme};
use crate::wrap::wrap;

fn gutter_width(theme: Theme) -> usize {
    crate::wrap::width(theme.glyph(Glyph::MessageGutter)) + 1
}

const fn role(who: Who, theme: Theme) -> (&'static str, ratatui::style::Color) {
    match who {
        Who::You => ("you", theme.border_active()),
        Who::Klod => ("klod", theme.border_idle()),
        Who::Error => ("err", theme.removed()),
        Who::Status => ("··", theme.muted()),
    }
}

/// One-line summary of what the call did: its path if it has one, never raw JSON.
pub(crate) fn summary(tool: &ToolBlock) -> String {
    let Some(raw) = tool.arguments.as_deref() else {
        return "arguments not sent over this transport".to_string();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return String::new();
    };
    for key in ["path", "file", "pattern", "query", "command"] {
        if let Some(found) = value.get(key).and_then(serde_json::Value::as_str) {
            return found.to_string();
        }
    }
    String::new()
}

fn mark(tool: &ToolBlock, theme: Theme) -> (String, ratatui::style::Color) {
    match &tool.status {
        ToolStatus::Running => (
            (*theme.spinner().first().unwrap_or(&"-")).to_string(),
            theme.secondary(),
        ),
        ToolStatus::Done { failed: true, .. } => {
            (theme.glyph(Glyph::ToolFailed).to_string(), theme.removed())
        }
        ToolStatus::Done { failed: false, .. } => {
            (theme.glyph(Glyph::ToolDone).to_string(), theme.added())
        }
        ToolStatus::Interrupted => (theme.glyph(Glyph::ToolFailed).to_string(), theme.warning()),
    }
}

/// Not routed through [`block`]'s message path: `wrap` collapses whitespace and would flatten the indents of pretty-printed arguments.
fn tool_block(tool: &ToolBlock, selected: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (glyph, rail) = if selected {
        (
            theme.glyph(Glyph::Caret),
            Style::default().fg(theme.border_active()),
        )
    } else {
        (
            theme.glyph(Glyph::MessageGutter),
            Style::default().fg(theme.muted()),
        )
    };
    let body_width = width.saturating_sub(gutter_width(theme));
    let rule = || Span::styled(format!("{glyph} "), rail);

    let (status, tint) = mark(tool, theme);
    let open = format!(
        "{} ",
        theme.glyph(if tool.expanded {
            Glyph::Expanded
        } else {
            Glyph::Collapsed
        })
    );
    let status = format!("{status} ");
    let mut head = vec![
        rule(),
        Span::styled(open.clone(), Style::default().fg(theme.muted())),
        Span::styled(status.clone(), Style::default().fg(tint)),
        Span::styled(tool.name.clone(), Style::default().fg(theme.body())),
    ];

    let mut spent = gutter_width(theme)
        + crate::wrap::width(&open)
        + crate::wrap::width(&status)
        + crate::wrap::width(&tool.name);
    let interrupted = tool.status == ToolStatus::Interrupted;
    if interrupted {
        spent += crate::wrap::width("  interrupted");
    }
    let did = summary(tool);
    let room = width.saturating_sub(spent + 2);
    if !did.is_empty() && room >= 4 {
        head.push(Span::styled(
            format!("  {}", elide(&did, room, theme.glyph(Glyph::Elided))),
            Style::default().fg(theme.secondary()),
        ));
    }
    if interrupted {
        head.push(Span::styled(
            "  interrupted".to_string(),
            Style::default().fg(theme.warning()),
        ));
    }
    let mut lines = vec![Line::from(head)];
    if !tool.expanded {
        return lines;
    }

    let surface = Style::default().fg(theme.body()).bg(theme.code_surface());
    if let Some(raw) = tool.arguments.as_deref() {
        lines.extend(
            crate::markdown::verbatim(&pretty(raw), body_width, "  ", surface)
                .into_iter()
                .map(|row| prefix(rule(), row)),
        );
    }
    if let ToolStatus::Done { content, failed } = &tool.status {
        let style = if *failed {
            Style::default()
                .fg(theme.removed())
                .bg(theme.code_surface())
        } else {
            surface
        };
        lines.push(Line::from(rule()));
        let body =
            crate::diff::render(content.trim_end(), body_width, theme).unwrap_or_else(|| {
                crate::markdown::verbatim(content.trim_end(), body_width, "  ", style)
            });
        lines.extend(body.into_iter().map(|row| prefix(rule(), row)));
    }
    lines
}

fn pretty(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .as_ref()
        .map_or_else(
            |_| raw.to_string(),
            |v| serde_json::to_string_pretty(v).unwrap_or_else(|_| raw.to_string()),
        )
}

fn elide(text: &str, max: usize, mark: &str) -> String {
    if crate::wrap::width(text) <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(crate::wrap::width(mark));
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for cluster in unicode_segmentation::UnicodeSegmentation::graphemes(text, true)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let cells = crate::wrap::width(cluster);
        if used + cells > room {
            break;
        }
        used += cells;
        kept.push(cluster);
    }
    kept.reverse();
    format!("{mark}{}", kept.concat())
}

fn prefix(rail: Span<'static>, row: Line<'static>) -> Line<'static> {
    let mut spans = vec![rail];
    spans.extend(row.spans);
    Line::from(spans)
}

/// Render one entry into lines, wrapped to `width` cells including the gutter.
#[must_use]
pub fn block(entry: &Entry, selected: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (who, text) = match entry {
        Entry::Message { who, text } => (*who, text.clone()),
        Entry::Tool(tool) => return tool_block(tool, selected, width, theme),
    };
    let (label, accent) = role(who, theme);
    let glyph = theme.glyph(Glyph::MessageGutter);
    let gutter = gutter_width(theme);
    let body_width = width.saturating_sub(gutter);

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(accent)),
        Span::styled(label.to_string(), Style::default().fg(theme.muted())),
    ])];

    let body: Vec<Line<'static>> = if who == Who::Klod {
        crate::markdown::render(&text, body_width, theme)
    } else {
        wrap(&text, body_width)
            .into_iter()
            .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.body()))))
            .collect()
    };

    for row in body {
        let mut spans = vec![Span::styled(
            format!("{glyph} "),
            Style::default().fg(accent),
        )];
        spans.extend(row.spans);
        lines.push(Line::from(spans));
    }
    lines
}

/// Render a whole transcript, with one blank row between blocks and none trailing.
#[must_use]
pub fn transcript(
    entries: &[Entry],
    cursor: Option<usize>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(String::new()));
        }
        lines.extend(block(entry, cursor == Some(i), width, theme));
    }
    lines
}

/// Where entry `index` starts in [`transcript`]'s output, and how many rows it takes.
#[must_use]
pub fn span_of(entries: &[Entry], index: usize, width: usize, theme: Theme) -> (usize, usize) {
    let mut start = 0;
    for entry in entries.iter().take(index) {
        start += block(entry, false, width, theme).len() + 1;
    }
    let height = entries
        .get(index)
        .map_or(0, |entry| block(entry, false, width, theme).len());
    (start, height)
}

/// The ask dialog's content, as lines.
#[must_use]
pub fn prompt_dialog(
    prompt: &Prompt,
    selected: Option<usize>,
    queue_len: usize,
    input: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if queue_len > 1 {
        lines.push(Line::from(Span::styled(
            format!("1 of {queue_len}"),
            Style::default().fg(theme.muted()),
        )));
    }
    lines.extend(
        wrap(&prompt.question, width)
            .into_iter()
            .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.body())))),
    );
    lines.push(Line::from(String::new()));

    if prompt.options.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.focus())),
            Span::styled(input.to_string(), Style::default().fg(theme.body())),
        ]));
        lines.push(Line::from(Span::styled(
            format!("empty answers with the default: {}", prompt.default),
            Style::default().fg(theme.muted()),
        )));
    } else {
        for (i, option) in prompt.options.iter().enumerate() {
            let is_selected = selected == Some(i);
            let is_default = *option == prompt.default;
            let marker = if is_selected {
                theme.glyph(Glyph::Caret)
            } else {
                " "
            };
            let row_style = Style::default().fg(if is_selected {
                theme.focus()
            } else {
                theme.body()
            });
            let mut spans = vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default().fg(if is_selected {
                        theme.focus()
                    } else {
                        theme.muted()
                    }),
                ),
                Span::styled(format!("{}. ", i + 1), Style::default().fg(theme.muted())),
                Span::styled(option.clone(), row_style),
            ];
            if is_default {
                spans.push(Span::styled(
                    format!(" {} default", theme.glyph(Glyph::Warning)),
                    Style::default().fg(theme.warning()),
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    lines.push(Line::from(String::new()));
    lines.push(Line::from(Span::styled(
        format!("silence takes the default: {}", prompt.default),
        Style::default().fg(theme.muted()),
    )));
    lines
}

/// Session switcher lines: the filter, then the matching sessions.
#[must_use]
pub fn sessions_dialog(
    query: &str,
    entries: &[&SessionEntry],
    selected: usize,
    current: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("filter: ", Style::default().fg(theme.muted())),
            Span::styled(query.to_string(), Style::default().fg(theme.body())),
        ]),
        Line::from(String::new()),
    ];
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "no session matches".to_string(),
            Style::default().fg(theme.muted()),
        )));
    } else {
        for (i, entry) in entries.iter().enumerate() {
            let is_selected = i == selected;
            let is_current = entry.id == current;
            let marker = if is_selected {
                theme.glyph(Glyph::Caret)
            } else {
                " "
            };
            let mut spans = vec![
                Span::styled(
                    format!("{marker} "),
                    Style::default().fg(if is_selected {
                        theme.focus()
                    } else {
                        theme.muted()
                    }),
                ),
                Span::styled(
                    entry.id.clone(),
                    Style::default().fg(if is_selected {
                        theme.focus()
                    } else {
                        theme.body()
                    }),
                ),
            ];
            if is_current {
                spans.push(Span::styled(
                    " (current)".to_string(),
                    Style::default().fg(theme.warning()),
                ));
            }
            let spent = crate::wrap::width(&entry.id) + if is_current { 10 } else { 0 } + 4;
            let room = width.saturating_sub(spent);
            if !entry.preview.is_empty() && room >= 4 {
                spans.push(Span::styled(
                    format!(
                        "  {}",
                        elide(&entry.preview, room, theme.glyph(Glyph::Elided))
                    ),
                    Style::default().fg(theme.secondary()),
                ));
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(String::new()));
    lines.push(Line::from(Span::styled(
        "type to filter · ↑/↓ choose · Enter switch · Esc close".to_string(),
        Style::default().fg(theme.muted()),
    )));
    lines
}

/// Help overlay lines, grouped by keymap context.
#[must_use]
pub fn help_dialog(width: usize, theme: Theme) -> Vec<Line<'static>> {
    const GROUPS: [(keymap::Context, &str); 5] = [
        (keymap::Context::Idle, "idle"),
        (keymap::Context::Streaming, "turn running"),
        (keymap::Context::Cancelling, "stopping"),
        (keymap::Context::DialogOpen, "dialog open"),
        (keymap::Context::DialogClosed, "dialog closed"),
    ];
    let mut lines = Vec::new();
    for (context, label) in GROUPS {
        let rows: Vec<&keymap::Binding> = keymap::BINDINGS
            .iter()
            .filter(|b| b.context == context)
            .collect();
        if rows.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::from(String::new()));
        }
        lines.push(Line::from(Span::styled(
            label.to_string(),
            Style::default().fg(theme.focus()),
        )));
        for binding in rows {
            let text = if binding.keys.is_empty() {
                binding.action.to_string()
            } else {
                format!("{}  {}", binding.keys, binding.action)
            };
            lines.extend(
                wrap(&text, width)
                    .into_iter()
                    .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.body())))),
            );
        }
    }
    lines
}

/// Quit confirm lines: session id, running-turn warning, cancel/quit options.
#[must_use]
#[allow(clippy::fn_params_excessive_bools)]
pub fn quit_dialog(
    session: &str,
    mid_turn: bool,
    turn_lost: bool,
    escalate: bool,
    quit_selected: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = wrap(
        &format!("session `{session}` is saved and can be resumed by id."),
        width,
    )
    .into_iter()
    .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.body()))))
    .collect();

    if mid_turn {
        let warning = if turn_lost {
            "a turn is still running — quitting stops the gateway with it, and \
             the turn is lost."
        } else {
            "a turn is still running on the gateway — quitting this client \
             leaves it running unaffected."
        };
        lines.push(Line::from(String::new()));
        lines.extend(
            wrap(warning, width)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.warning())))),
        );
    }
    if escalate {
        lines.push(Line::from(String::new()));
        lines.extend(
            wrap("quit again to confirm.", width)
                .into_iter()
                .map(|row| Line::from(Span::styled(row, Style::default().fg(theme.warning())))),
        );
    }

    lines.push(Line::from(String::new()));
    for (i, (label, is_quit)) in [("cancel", false), ("quit", true)].into_iter().enumerate() {
        let is_selected = quit_selected == is_quit;
        let marker = if is_selected {
            theme.glyph(Glyph::Caret)
        } else {
            " "
        };
        let mut spans = vec![
            Span::styled(
                format!("{marker} "),
                Style::default().fg(if is_selected {
                    theme.focus()
                } else {
                    theme.muted()
                }),
            ),
            Span::styled(
                format!("{}. {label}", i + 1),
                Style::default().fg(if is_selected {
                    theme.focus()
                } else {
                    theme.body()
                }),
            ),
        ];
        if !is_quit {
            spans.push(Span::styled(
                format!(" {} default", theme.glyph(Glyph::Warning)),
                Style::default().fg(theme.warning()),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{block, span_of, transcript};
    use crate::app::{Entry, ToolBlock, ToolStatus, Who};
    use crate::theme::{Depth, Glyph, GlyphSet, Mode, Theme};
    use crate::wrap::width;

    const ALL: [Who; 4] = [Who::You, Who::Klod, Who::Error, Who::Status];

    fn done(content: &str) -> ToolStatus {
        ToolStatus::Done {
            content: content.to_string(),
            failed: false,
        }
    }

    fn failure(content: &str) -> ToolStatus {
        ToolStatus::Done {
            content: content.to_string(),
            failed: true,
        }
    }

    fn entry(who: Who, text: &str) -> Entry {
        Entry::Message {
            who,
            text: text.to_string(),
        }
    }

    fn plain(line: &ratatui::text::Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn every_role_renders_its_label_and_its_gutter() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let glyph = theme.glyph(Glyph::MessageGutter);
        for who in ALL {
            let lines = block(&entry(who, "a message"), false, 40, theme);
            let head = plain(&lines[0]);
            assert!(
                head.starts_with(glyph),
                "{who:?}: the label row has no gutter: {head:?}"
            );
            assert!(
                head.trim_start_matches(glyph).trim().len() >= 2,
                "{who:?}: no role label: {head:?}"
            );
            for line in &lines[1..] {
                assert!(
                    plain(line).starts_with(glyph),
                    "{who:?}: body lost the gutter"
                );
            }
        }
    }

    #[test]
    fn monochrome_keeps_the_glyph_and_the_label() {
        let theme = Theme::new(Mode::Mono, Depth::TrueColor, GlyphSet::Unicode);
        let glyph = theme.glyph(Glyph::MessageGutter);
        for who in ALL {
            let lines = block(&entry(who, "a message"), false, 40, theme);
            let head = plain(&lines[0]);
            assert!(head.starts_with(glyph), "{who:?}: no gutter under Mono");
            assert!(
                head.trim_start_matches(glyph).trim().len() >= 2,
                "{who:?}: no label under Mono, so nothing identifies the speaker"
            );
        }
        let labels: Vec<String> = ALL
            .into_iter()
            .map(|who| plain(&block(&entry(who, "x"), false, 40, theme)[0]))
            .collect();
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert_ne!(a, b, "two roles read identically under Mono");
            }
        }
    }

    #[test]
    fn no_rendered_line_exceeds_the_pane_including_the_gutter() {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Dark, Depth::TrueColor, set);
            let long = "日本語のテキストです ".repeat(6);
            let lines = transcript(
                &[entry(Who::You, &long), entry(Who::Klod, &long)],
                None,
                24,
                theme,
            );
            for line in &lines {
                let rendered = plain(line);
                assert!(
                    width(&rendered) <= 24,
                    "{set:?}: {rendered:?} is {} cells, pane is 24",
                    width(&rendered)
                );
            }
        }
    }

    #[test]
    fn blocks_are_separated_by_one_blank_row_and_do_not_trail_one() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let lines = transcript(
            &[entry(Who::You, "one"), entry(Who::Klod, "two")],
            None,
            40,
            theme,
        );
        let blanks = lines.iter().filter(|l| plain(l).is_empty()).count();
        assert_eq!(
            blanks, 1,
            "one separator between two blocks, and no trailer"
        );
        assert!(
            !plain(lines.last().expect("lines")).is_empty(),
            "a trailing blank row would be a special case for the viewport"
        );
        assert!(
            transcript(&[], None, 40, theme).is_empty(),
            "no entries, no lines"
        );
    }

    fn call(name: &str, arguments: Option<&str>, status: ToolStatus) -> Entry {
        Entry::Tool(ToolBlock {
            id: "c1".to_string(),
            name: name.to_string(),
            arguments: arguments.map(str::to_string),
            status,
            expanded: false,
        })
    }

    fn opened(entry: Entry) -> Entry {
        let Entry::Tool(mut tool) = entry else {
            unreachable!("not a tool block")
        };
        tool.expanded = true;
        Entry::Tool(tool)
    }

    #[test]
    fn the_collapsed_line_names_the_path_and_never_the_json() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let edit = call(
            "edit",
            Some(r#"{"path":"pkgs/clients/tui/src/blocks.rs","old":"a","new":"b"}"#),
            done("ok"),
        );
        let lines = block(&edit, false, 80, theme);
        assert_eq!(lines.len(), 1, "collapsed is one row");
        let head = plain(&lines[0]);
        assert!(head.contains("edit"), "the tool is not named: {head:?}");
        assert!(
            head.contains("pkgs/clients/tui/src/blocks.rs"),
            "the path is what the reader came for: {head:?}"
        );
        assert!(
            !head.contains('{') && !head.contains("\"old\""),
            "the collapsed line is showing raw JSON: {head:?}"
        );
        assert!(
            head.contains(theme.glyph(Glyph::ToolDone)),
            "no status glyph: {head:?}"
        );

        let bare = call("list_sessions", Some("{}"), done("ok"));
        let bare = plain(&block(&bare, false, 80, theme)[0]);
        assert!(
            bare.contains("list_sessions") && !bare.contains("{}"),
            "{bare:?}"
        );
    }

    #[test]
    fn missing_arguments_say_so_instead_of_reading_as_none() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let head = plain(&block(&call("read", None, ToolStatus::Running), false, 80, theme)[0]);
        assert!(
            head.contains("not sent"),
            "a user cannot tell missing from absent without the reason: {head:?}"
        );
    }

    #[test]
    fn a_failed_call_shows_its_reason_without_a_keystroke() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let failed = opened(call(
            "read",
            Some(r#"{"path":"missing.txt"}"#),
            failure("tool `read` error: NotFound"),
        ));
        let rendered: Vec<String> = block(&failed, false, 80, theme).iter().map(plain).collect();
        assert!(
            rendered[0].contains(theme.glyph(Glyph::ToolFailed)),
            "a failure wears the success glyph: {:?}",
            rendered[0]
        );
        assert!(
            rendered.iter().any(|r| r.contains("NotFound")),
            "the reason is not in the block: {rendered:?}"
        );
    }

    #[test]
    fn what_the_content_looks_like_no_longer_decides_whether_a_call_failed() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let head = |status| plain(&block(&call("read", None, status), false, 80, theme)[0]);

        let innocent = head(done("the file mentions tool call denied: in a comment"));
        assert!(
            innocent.contains(theme.glyph(Glyph::ToolDone)),
            "a tool's own output that merely quotes a denial was drawn as a \
             failure: {innocent:?}"
        );

        let reworded = head(failure("the sandbox said no"));
        assert!(
            reworded.contains(theme.glyph(Glyph::ToolFailed)),
            "a failure the core phrased in words no matcher would have listed \
             was drawn as a success: {reworded:?}"
        );
    }

    #[test]
    fn expanded_arguments_keep_their_shape_and_the_result_follows() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let entry = opened(call(
            "edit",
            Some(r#"{"path":"a.rs","new":"b"}"#),
            done("wrote 1 line"),
        ));
        let rendered: Vec<String> = block(&entry, false, 80, theme).iter().map(plain).collect();
        assert!(rendered.len() > 4, "expanded shows its work: {rendered:?}");
        assert!(
            rendered.iter().any(|r| r.contains("\"path\": \"a.rs\"")),
            "the arguments are not pretty-printed: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|r| r.contains("wrote 1 line")),
            "the result is missing: {rendered:?}"
        );
        assert!(
            plain(
                &block(
                    &call("edit", Some("{}"), ToolStatus::Running),
                    false,
                    80,
                    theme
                )[0]
            )
            .contains(theme.glyph(Glyph::Collapsed)),
            "collapsed and expanded read the same"
        );
    }

    #[test]
    fn monochrome_still_separates_a_done_call_from_a_failed_one() {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Mono, Depth::TrueColor, set);
            let args = Some(r#"{"path":"a.rs"}"#);
            let done = plain(&block(&call("read", args, done("ok")), false, 60, theme)[0]);
            let failed = plain(
                &block(
                    &call("read", args, failure("tool `read` trapped")),
                    false,
                    60,
                    theme,
                )[0],
            );
            let running =
                plain(&block(&call("read", args, ToolStatus::Running), false, 60, theme)[0]);
            assert_ne!(done, failed, "{set:?}: a failed call reads as a done one");
            assert_ne!(done, running, "{set:?}: a running call reads as a done one");
            assert!(
                failed.contains(theme.glyph(Glyph::ToolFailed)),
                "{set:?}: {failed:?}"
            );
        }
    }

    #[test]
    fn a_result_that_is_a_diff_renders_as_one_even_when_the_call_failed() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let patch = "@@ -1,2 +1,2 @@\n context\n-gone\n+new";
        let rendered: Vec<String> = block(
            &opened(call("git", Some(r#"{"op":"diff"}"#), done(patch))),
            false,
            60,
            theme,
        )
        .iter()
        .map(plain)
        .collect();
        let gutter = theme.glyph(Glyph::MessageGutter);
        let body: Vec<String> = rendered
            .iter()
            .map(|r| {
                r.trim_start_matches(gutter)
                    .trim_start_matches(' ')
                    .to_string()
            })
            .collect();
        assert!(
            body.iter().any(|r| r.starts_with("+  2 new")),
            "the result did not go through the diff renderer: {body:?}"
        );

        let failed = opened(call(
            "git",
            Some("{}"),
            failure(&format!("tool `git` error: {patch}")),
        ));
        let head = plain(&block(&failed, false, 60, theme)[0]);
        assert!(head.contains(theme.glyph(Glyph::ToolFailed)), "{head:?}");
    }

    #[test]
    fn the_selected_block_is_marked_by_a_glyph_so_monochrome_keeps_it() {
        for mode in [Mode::Dark, Mode::Mono] {
            for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
                let theme = Theme::new(mode, Depth::TrueColor, set);
                let entry = call("read", Some("{}"), ToolStatus::Running);
                let idle = plain(&block(&entry, false, 40, theme)[0]);
                let picked = plain(&block(&entry, true, 40, theme)[0]);
                assert_ne!(
                    idle, picked,
                    "{mode:?}/{set:?}: the cursor is invisible without colour"
                );
                assert!(
                    picked.starts_with(theme.glyph(Glyph::Caret)),
                    "{mode:?}/{set:?}: {picked:?}"
                );
                assert_eq!(
                    width(&idle),
                    width(&picked),
                    "{mode:?}/{set:?}: selecting a block reflowed it"
                );
            }
        }
    }

    #[test]
    fn span_of_finds_each_block_where_the_transcript_actually_put_it() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let entries = [
            entry(Who::You, "a question that is long enough to wrap over rows"),
            call("read", Some(r#"{"path":"a.rs"}"#), ToolStatus::Running),
            entry(Who::Klod, "an answer"),
        ];
        let all = transcript(&entries, None, 30, theme);
        for (i, expected) in entries.iter().enumerate() {
            let (start, rows) = span_of(&entries, i, 30, theme);
            let drawn: Vec<String> = all[start..start + rows].iter().map(plain).collect();
            let alone: Vec<String> = block(expected, false, 30, theme)
                .iter()
                .map(plain)
                .collect();
            assert_eq!(drawn, alone, "entry {i} is not where `span_of` says it is");
        }
        assert_eq!(span_of(&entries, 9, 30, theme).1, 0, "no entry, no rows");
    }

    #[test]
    fn an_expanded_block_stays_inside_the_pane() {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Dark, Depth::TrueColor, set);
            let entry = opened(call(
                "grep",
                Some(r#"{"pattern":"a very long pattern that will not fit on one row at all"}"#),
                done(&"日本語のテキストです ".repeat(4)),
            ));
            for line in block(&entry, false, 24, theme) {
                let row = plain(&line);
                assert!(
                    width(&row) <= 24,
                    "{set:?}: {row:?} is {} cells, pane is 24",
                    width(&row)
                );
            }
        }
    }

    fn ask(session: &str, question: &str, options: &[&str], default: &str) -> crate::app::Prompt {
        crate::app::Prompt {
            session: session.to_string(),
            question: question.to_string(),
            options: options.iter().map(|s| (*s).to_string()).collect(),
            default: default.to_string(),
        }
    }

    #[test]
    fn the_dialog_says_what_silence_means_and_names_the_default() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let prompt = ask("s1", "write src/main.rs?", &["yes", "no"], "no");
        let rendered: Vec<String> = super::prompt_dialog(&prompt, Some(1), 1, "", 60, theme)
            .iter()
            .map(plain)
            .collect();
        assert!(
            rendered
                .iter()
                .any(|r| r.to_lowercase().contains("silence") && r.contains("no")),
            "no line explains what happens if nobody answers: {rendered:?}"
        );
    }

    #[test]
    fn monochrome_still_marks_the_default_option_in_text() {
        let theme = Theme::new(Mode::Mono, Depth::TrueColor, GlyphSet::Unicode);
        let prompt = ask("s1", "which?", &["always", "yes", "no"], "no");
        let lines = super::prompt_dialog(&prompt, Some(2), 1, "", 60, theme);
        let rendered: Vec<String> = lines.iter().map(plain).collect();

        let default_row = rendered
            .iter()
            .find(|r| r.contains("no"))
            .expect("the default option's row is rendered");
        assert!(
            default_row.contains("default"),
            "the default is not named in the row's text, only (perhaps) in \
             colour that Mode::Mono has already zeroed out: {default_row:?}"
        );
        for other in ["always", "yes"] {
            let row = rendered
                .iter()
                .find(|r| r.contains(other))
                .unwrap_or_else(|| panic!("{other}'s row is rendered"));
            assert!(
                !row.contains("default"),
                "{other} is not the default and must not read as one: {row:?}"
            );
        }
    }

    #[test]
    fn the_selected_option_renders_differently_from_the_rest() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let prompt = ask("s1", "which?", &["yes", "no"], "no");
        let rendered: Vec<String> = super::prompt_dialog(&prompt, Some(0), 1, "", 60, theme)
            .iter()
            .map(plain)
            .collect();
        let yes_row = rendered.iter().find(|r| r.contains("yes")).unwrap();
        let no_row = rendered.iter().find(|r| r.contains("no")).unwrap();
        assert_ne!(
            yes_row, no_row,
            "the selected row must not read the same as an unselected one"
        );
    }

    #[test]
    fn an_empty_options_list_renders_free_text_input() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let prompt = ask("s1", "say something", &[], "nothing");
        let rendered: Vec<String> =
            super::prompt_dialog(&prompt, None, 1, "typed so far", 60, theme)
                .iter()
                .map(plain)
                .collect();
        assert!(
            rendered.iter().any(|r| r.contains("typed so far")),
            "the composer's text is not shown: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|r| r.contains("nothing")),
            "the default is not named as what an empty answer sends: {rendered:?}"
        );
    }

    fn session_entry(id: &str, preview: &str) -> crate::app::SessionEntry {
        crate::app::SessionEntry {
            id: id.to_string(),
            preview: preview.to_string(),
        }
    }

    #[test]
    fn the_current_session_is_marked_in_text_under_monochrome() {
        let theme = Theme::new(Mode::Mono, Depth::TrueColor, GlyphSet::Unicode);
        let a = session_entry("cli", "hello");
        let b = session_entry("other", "world");
        let refs = vec![&a, &b];
        let rendered: Vec<String> = super::sessions_dialog("", &refs, 0, "other", 60, theme)
            .iter()
            .map(plain)
            .collect();
        let cli_row = rendered.iter().find(|r| r.contains("cli")).unwrap();
        let other_row = rendered.iter().find(|r| r.contains("other")).unwrap();
        assert!(!cli_row.contains("current"), "{cli_row:?}");
        assert!(other_row.contains("current"), "{other_row:?}");
    }

    #[test]
    fn an_empty_session_list_says_so() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let rendered: Vec<String> = super::sessions_dialog("", &[], 0, "cli", 60, theme)
            .iter()
            .map(plain)
            .collect();
        assert!(rendered.iter().any(|r| r.contains("no session matches")));
    }

    #[test]
    fn the_help_overlay_lists_every_binding_grouped_by_context() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let rendered: Vec<String> = super::help_dialog(60, theme).iter().map(plain).collect();
        let joined = rendered.join("\n");
        for binding in crate::keymap::BINDINGS {
            if !binding.keys.is_empty() {
                assert!(
                    joined.contains(binding.keys),
                    "{:?} is missing from the overlay",
                    binding.keys
                );
            }
            assert!(
                joined.contains(binding.action),
                "{:?} is missing from the overlay",
                binding.action
            );
        }
        for label in [
            "idle",
            "turn running",
            "stopping",
            "dialog open",
            "dialog closed",
        ] {
            assert!(
                rendered.iter().any(|r| r == label),
                "the {label:?} group heading is missing: {rendered:?}"
            );
        }
    }

    #[test]
    fn the_quit_dialog_wording_differs_between_stdio_and_addr() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let stdio = super::quit_dialog("cli", true, true, false, false, 60, theme)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        let addr = super::quit_dialog("cli", true, false, false, false, 60, theme)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(stdio, addr, "the two transports must not read alike");
        assert!(stdio.contains("lost"), "{stdio:?}");
        assert!(!addr.contains("lost"), "{addr:?}");
    }

    #[test]
    fn a_quit_with_no_turn_running_carries_no_warning() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let rendered: Vec<String> =
            super::quit_dialog("cli", false, false, false, false, 60, theme)
                .iter()
                .map(plain)
                .collect();
        assert!(!rendered.iter().any(|r| r.contains("running")));
        let cancel_row = rendered.iter().find(|r| r.contains("cancel")).unwrap();
        assert!(
            cancel_row.contains("default"),
            "cancel must be the default selection: {cancel_row:?}"
        );
    }

    #[test]
    fn escalating_adds_a_second_warning() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let first: Vec<String> = super::quit_dialog("cli", true, true, false, true, 60, theme)
            .iter()
            .map(plain)
            .collect();
        let second: Vec<String> = super::quit_dialog("cli", true, true, true, true, 60, theme)
            .iter()
            .map(plain)
            .collect();
        assert!(!first.iter().any(|r| r.contains("again")));
        assert!(second.iter().any(|r| r.contains("again")));
    }

    #[test]
    fn the_queue_count_only_shows_when_more_than_one_is_waiting() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let prompt = ask("s1", "which?", &["yes", "no"], "no");
        let alone: Vec<String> = super::prompt_dialog(&prompt, Some(1), 1, "", 60, theme)
            .iter()
            .map(plain)
            .collect();
        assert!(
            !alone.iter().any(|r| r.contains("of")),
            "a single pending prompt must not claim to be part of a queue: {alone:?}"
        );

        let queued: Vec<String> = super::prompt_dialog(&prompt, Some(1), 2, "", 60, theme)
            .iter()
            .map(plain)
            .collect();
        assert!(
            queued.iter().any(|r| r.contains("1 of 2")),
            "a second prompt waiting must say so: {queued:?}"
        );
    }
}
