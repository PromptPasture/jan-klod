//! A message is a block, not a line (#147).
//!
//! Turns a transcript and a pane width into rendered lines. It draws nothing,
//! which is what lets every test here run without a terminal and what lets
//! #148's viewport scroll over the result: lines are data, and deciding which
//! of them are on screen is a separate question from producing them.
//!
//! # The layout
//!
//! Each entry is a gutter glyph column, a one-line role label, then the wrapped
//! body — and one blank row between blocks, so a transcript reads as messages
//! rather than as a wall.
//!
//! ```text
//! ▍ you
//! ▍ the message, wrapped to the pane and
//! ▍ indented under its own gutter
//!
//! ▍ klod
//! ▍ the answer
//! ```
//!
//! # A tool call is a block too, and it is one row until it is not (#155)
//!
//! It has no role label. The row *is* the label: an open/closed marker, a
//! status glyph, the tool's name, and what the call did — a path, not its JSON,
//! because the collapsed form exists to be scanned down.
//!
//! ```text
//! ▍ ▸ ✓ edit  src/core/ui/src/blocks.rs
//! ▍ ▾ ✗ read  missing.txt
//! ▍   {
//! ▍     "path": "missing.txt"
//! ▍   }
//! ▍
//! ▍   tool `read` error: NotFound
//! ```
//!
//! **A failure opens itself.** Everything else stays shut, because a transcript
//! of twelve successful reads is not improved by twelve screens of JSON, and
//! the one block a user certainly wants to read is the one that went wrong —
//! putting its reason behind a keystroke they may not know about is the wrong
//! way round.
//!
//! Opening one by hand is `Ctrl+O`, acting on the block under the transcript
//! cursor (`App::cursor`, moved with `Alt+Up`/`Alt+Down`). A selected block is
//! marked by swapping its gutter glyph rather than its colour, which is the
//! same rule as everything else here: the mark has to survive `Mode::Mono`.
//!
//! # Every distinction survives monochrome
//!
//! Under [`Mode::Mono`](crate::theme::Mode::Mono) every role resolves to
//! `Color::Reset`, so colour carries nothing and the glyph plus the role label
//! are the whole signal. That is not a degraded mode — the design treats colour
//! as the *second* signal throughout — but it is a rule this module has to keep
//! rather than inherit, which is why the tests assert the label and the glyph
//! are present with the theme in `Mono`.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::{Entry, ToolBlock, ToolStatus, Who};
use crate::theme::{Glyph, Theme};
use crate::wrap::wrap;

/// The gutter column: a glyph and the space after it.
///
/// Everything the body is wrapped into has to leave room for this, or the first
/// wrapped line fits the pane and the rendered line does not.
fn gutter_width(theme: Theme) -> usize {
    crate::wrap::width(theme.glyph(Glyph::MessageGutter)) + 1
}

/// The role label and the colour its gutter takes.
///
/// `Error` is `removed` and `Status` is `muted` — both exact, since a client
/// error *is* a failure and a status note *is* recessive. The two speakers take
/// the border roles rather than an accent: #97's rule is that user and assistant
/// are distinguished by gutter and indent rather than by tint, and an accent
/// here would spend one of the four meanings on "who is talking".
const fn role(who: Who, theme: Theme) -> (&'static str, ratatui::style::Color) {
    match who {
        Who::You => ("you", theme.border_active()),
        Who::Klod => ("klod", theme.border_idle()),
        Who::Error => ("err", theme.removed()),
        Who::Status => ("··", theme.muted()),
    }
}

/// Whether a tool result reads as a failure.
///
/// **A convention, not a contract, and that distinction is the point.** Nothing
/// on the wire says a tool call failed: `ToolOutcome` is `{ id, content }`,
/// `ToolInvoker::invoke` returns `Option<String>`, and both frames carry only
/// the content. So a client cannot be *told* that a call failed; it can only
/// recognise the sentences the core writes when one does, which is what this
/// does — and it will stop working the day somebody rephrases an error without
/// touching this file.
///
/// The whole corpus, at the time of writing:
///
/// | sentence | written by |
/// | --- | --- |
/// | ``tool `X` error: …`` | `tool_host.rs` |
/// | ``tool `X` trapped`` | `tool_host.rs` |
/// | ``tool `X` meta trapped`` | `tool_host.rs` |
/// | `tool fleet failed to instantiate: …` | `tool_host.rs` |
/// | `tool call denied: …` | `conductor.rs` |
/// | ``no tool named `X` `` | `conductor.rs` |
///
/// It lives in one named function for exactly that reason:
/// [#162](https://github.com/PromptPasture/jan-klod/issues/162) puts the flag on
/// the wire and deletes this, and there should be one place to delete rather
/// than a scatter of `contains` calls behind a rendering decision.
#[must_use]
pub fn reads_as_failure(content: &str) -> bool {
    let head = content.trim_start();
    // These stand alone.
    for opener in [
        "tool call denied:",
        "no tool named `",
        "tool fleet failed to instantiate:",
    ] {
        if head.starts_with(opener) {
            return true;
        }
    }
    // These need the ``tool `name` `` opener, because "error:" on its own is a
    // word a successful tool could easily have printed.
    head.starts_with("tool `")
        && ["` error:", "` trapped", "` meta trapped"]
            .iter()
            .any(|tail| head.contains(tail))
}

/// The one-line summary: what the call *did*, not its JSON.
///
/// A path if the arguments name one, because that is what the reader of an
/// `edit` or a `read` came for. Anything else returns nothing and the line is
/// just the tool's name — the collapsed form exists to be scanned, and a JSON
/// blob squeezed onto one row is not scannable.
fn summary(tool: &ToolBlock) -> String {
    let Some(raw) = tool.arguments.as_deref() else {
        // Not the same thing as "no arguments". The SSE `tool` frame does not
        // carry them at all ([#161]), so over REST this is *missing*
        // information, and a line that rendered `{}` here would be telling a
        // user something false that they have no way to check.
        //
        // [#161]: https://github.com/PromptPasture/jan-klod/issues/161
        return "arguments not sent over this transport (#161)".to_string();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return String::new();
    };
    // In precedence order, not the JSON's order: a call with both a `path` and
    // a `pattern` is a search *in* that file, and the file is the noun.
    for key in ["path", "file", "pattern", "query", "command"] {
        if let Some(found) = value.get(key).and_then(serde_json::Value::as_str) {
            return found.to_string();
        }
    }
    String::new()
}

/// The status glyph and the colour it carries.
fn mark(tool: &ToolBlock, theme: Theme) -> (String, ratatui::style::Color) {
    match &tool.status {
        // The first spinner frame, standing still. `block` is a pure function
        // of the model and has no tick to animate against; #160 is the slice
        // that gives the client one, and this is the frame it starts from.
        ToolStatus::Running => (
            (*theme.spinner().first().unwrap_or(&"-")).to_string(),
            theme.secondary(),
        ),
        ToolStatus::Done(content) if reads_as_failure(content) => {
            (theme.glyph(Glyph::ToolFailed).to_string(), theme.removed())
        }
        ToolStatus::Done(_) => (theme.glyph(Glyph::ToolDone).to_string(), theme.added()),
        ToolStatus::Interrupted => (theme.glyph(Glyph::ToolFailed).to_string(), theme.warning()),
    }
}

/// A tool call: one scannable line, or that line plus its work.
///
/// This does **not** go through [`block`]'s message path, and the reason is
/// mechanical rather than stylistic — [`crate::wrap::wrap`] collapses runs of
/// whitespace, which is right for prose and would silently flatten every
/// indent out of the pretty-printed arguments below.
fn tool_block(tool: &ToolBlock, selected: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    // The cursor is a **different glyph**, not a colour: under `Mode::Mono`
    // `border_active()` and `muted()` are both `Color::Reset`, so a selection
    // drawn in colour would be a selection that does not exist on the one mode
    // with none to spend. Both glyphs are one cell wide in both vocabularies,
    // so selecting a block cannot reflow it.
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

    // The collapsed form is **one row**, so what does not fit is cut rather than
    // wrapped: a call that needed three rows to say it was a call would defeat
    // the point of collapsing it. The name and the status are never the thing
    // cut — they are what the row is for — so only the summary is squeezed, and
    // it is cut from the *left*, because the informative end of a path is the
    // filename and the informative end of a command is rarely the binary.
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

    // Indented **and** on `code_surface`, because the surface alone is
    // `Color::Reset` under `Mode::Mono` — a block that separated itself by
    // background would vanish on the one mode that has no colour to spend.
    // The same pairing is why fenced code reads as code in `markdown.rs`.
    let surface = Style::default().fg(theme.body()).bg(theme.code_surface());
    if let Some(raw) = tool.arguments.as_deref() {
        lines.extend(
            crate::markdown::verbatim(&pretty(raw), body_width, "  ", surface)
                .into_iter()
                .map(|row| prefix(rule(), row)),
        );
    }
    if let ToolStatus::Done(content) = &tool.status {
        let style = if reads_as_failure(content) {
            // The reason a failure opens itself is that it is *read*, not
            // glanced at, so it gets the failure colour rather than the
            // recessive one every other result has.
            Style::default()
                .fg(theme.removed())
                .bg(theme.code_surface())
        } else {
            surface
        };
        lines.push(Line::from(rule()));
        // A diff renders as a diff, **including inside a failed result** (#156).
        // The two claims collide — a failure is drawn in `removed()`, which is
        // the role a `-` line wants — and the diff wins, because a diff whose
        // every line is the removal colour says the opposite of what it is. The
        // failure is already said twice on the head row, by glyph and by
        // colour, and neither of those is what the reader is squinting at here.
        //
        // Not indented, unlike the arguments above: the sign has to reach
        // column 0 of the body or `Mode::Mono` has nothing to read it by, and
        // a diff needs no indent to be recognisable as one.
        let body =
            crate::diff::render(content.trim_end(), body_width, theme).unwrap_or_else(|| {
                crate::markdown::verbatim(content.trim_end(), body_width, "  ", style)
            });
        lines.extend(body.into_iter().map(|row| prefix(rule(), row)));
    }
    lines
}

/// The arguments as a human reads them, or unchanged when they are not JSON.
///
/// Pretty-printed and **not** highlighted: 19b priced four highlighters against
/// this crate's dependency budget and declined all of them, and one tool block
/// is not the argument that reopens it.
fn pretty(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .as_ref()
        .map_or_else(
            |_| raw.to_string(),
            |v| serde_json::to_string_pretty(v).unwrap_or_else(|_| raw.to_string()),
        )
}

/// `text` cut to `max` cells from the left, marked so the cut is visible.
///
/// Grapheme by grapheme rather than by byte or by `char`, for the same reason
/// `wrap` is: a cut inside a cluster is a broken glyph, and a cut counted in
/// `char`s puts a wide character half off the pane.
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

/// Put the gutter back in front of an already-styled row.
fn prefix(rail: Span<'static>, row: Line<'static>) -> Line<'static> {
    let mut spans = vec![rail];
    spans.extend(row.spans);
    Line::from(spans)
}

/// Render one entry into lines/// Render one entry into lines, wrapped to `width` cells including the gutter.
#[must_use]
pub fn block(entry: &Entry, selected: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (who, text) = match entry {
        // `selected` is not passed on here, and that is the model rather than an
        // omission: the cursor only ever lands on a tool block, because a
        // message has nothing to open.
        Entry::Message { who, text } => (*who, text.clone()),
        Entry::Tool(tool) => return tool_block(tool, selected, width, theme),
    };
    let (label, accent) = role(who, theme);
    let glyph = theme.glyph(Glyph::MessageGutter);
    let gutter = gutter_width(theme);
    let body_width = width.saturating_sub(gutter);

    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(accent)),
        // The label is `muted` and the body is `body`: `muted` has no contrast
        // floor on a light terminal (#135), so it is fine for a one-word label
        // and would not be for the message itself.
        Span::styled(label.to_string(), Style::default().fg(theme.muted())),
    ])];

    // Markdown for the assistant and nobody else (#149). A user's own message is
    // shown as they typed it — rendering it would mean their backticks and
    // asterisks disappearing from their own transcript — and an error or a
    // status note is not a document.
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

/// Render a whole transcript, one blank row between blocks.
///
/// The blank row goes *between* rather than after, so the last block does not
/// leave a trailing row the viewport would have to special-case.
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

/// Where entry `index` starts in what [`transcript`] renders, and how tall it
/// is.
///
/// The caller is the scroll: a cursor that moves to a block the pane is not
/// showing has selected something the user cannot see, which is worse than no
/// cursor at all. Measured by rendering, because the only honest answer to
/// "how many rows is this" in a wrapping layout is to wrap it — and rendering
/// here is a pure function, so this costs what a frame costs.
///
/// Measured **unselected**, and that is safe rather than approximate: the
/// cursor swaps one one-cell glyph for another, so it cannot change where the
/// body wraps.
#[must_use]
pub fn span_of(entries: &[Entry], index: usize, width: usize, theme: Theme) -> (usize, usize) {
    let mut start = 0;
    for entry in entries.iter().take(index) {
        // `+ 1` for the blank row `transcript` puts *between* blocks.
        start += block(entry, false, width, theme).len() + 1;
    }
    let height = entries
        .get(index)
        .map_or(0, |entry| block(entry, false, width, theme).len());
    (start, height)
}

#[cfg(test)]
mod tests {
    use super::{block, reads_as_failure, span_of, transcript};
    use crate::app::{Entry, ToolBlock, ToolStatus, Who};
    use crate::theme::{Depth, Glyph, GlyphSet, Mode, Theme};
    use crate::wrap::width;

    const ALL: [Who; 4] = [Who::You, Who::Klod, Who::Error, Who::Status];

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

    /// The rule the whole design rests on: with colour gone, the block is still
    /// readable and still attributed.
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
        // And the four labels are still four different strings, so colour was
        // never the thing telling them apart.
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

    /// The gutter is part of the line, so the wrap has to leave room for it.
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

    /// Acceptance: the collapsed line names what the call *did*.
    #[test]
    fn the_collapsed_line_names_the_path_and_never_the_json() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let edit = call(
            "edit",
            Some(r#"{"path":"src/core/ui/src/blocks.rs","old":"a","new":"b"}"#),
            ToolStatus::Done("ok".to_string()),
        );
        let lines = block(&edit, false, 80, theme);
        assert_eq!(lines.len(), 1, "collapsed is one row");
        let head = plain(&lines[0]);
        assert!(head.contains("edit"), "the tool is not named: {head:?}");
        assert!(
            head.contains("src/core/ui/src/blocks.rs"),
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

        // Nothing to name, so the line is just the call.
        let bare = call("list_sessions", Some("{}"), ToolStatus::Done("ok".into()));
        let bare = plain(&block(&bare, false, 80, theme)[0]);
        assert!(
            bare.contains("list_sessions") && !bare.contains("{}"),
            "{bare:?}"
        );
    }

    /// #161: over REST the frame carries no arguments at all, and the line has
    /// to say that rather than render an empty call.
    #[test]
    fn missing_arguments_say_so_instead_of_reading_as_none() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let head = plain(&block(&call("read", None, ToolStatus::Running), false, 80, theme)[0]);
        assert!(
            head.contains("#161"),
            "a user cannot tell missing from absent without the reason: {head:?}"
        );
    }

    /// Acceptance: a failure opens itself, and the reason is in the block.
    #[test]
    fn a_failed_call_shows_its_reason_without_a_keystroke() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let failed = opened(call(
            "read",
            Some(r#"{"path":"missing.txt"}"#),
            ToolStatus::Done("tool `read` error: NotFound".to_string()),
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

    /// The wording this recognises belongs to another crate, so the day it
    /// changes there, this is the test that says so.
    #[test]
    fn every_failure_the_core_writes_is_recognised() {
        for content in [
            "tool `read` error: NotFound",
            "tool `read` trapped",
            "tool `read` meta trapped",
            "tool fleet failed to instantiate: no such file",
            "tool call denied: the user said no",
            "no tool named `edti`",
        ] {
            assert!(
                reads_as_failure(content),
                "{content:?} is a failure the core emits and this did not see it \
                 — check `tool_host.rs` and `conductor.rs` before editing the list"
            );
        }
        for content in [
            "ok",
            "",
            "error: 1 test failed",
            "the file mentions tool call denied: in a comment",
        ] {
            assert!(
                !reads_as_failure(content),
                "{content:?} is a tool's own output and was read as the tool failing"
            );
        }
    }

    /// The reason `tool_block` does not go through the message path: `wrap`
    /// collapses whitespace, so the arguments would arrive with their structure
    /// silently pressed flat.
    #[test]
    fn expanded_arguments_keep_their_shape_and_the_result_follows() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let entry = opened(call(
            "edit",
            Some(r#"{"path":"a.rs","new":"b"}"#),
            ToolStatus::Done("wrote 1 line".to_string()),
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

    /// Acceptance: with colour gone, done and failed are still two things.
    ///
    /// The whole reason status is a glyph rather than a tint — and a case the
    /// *message* blocks' `Mono` test cannot cover, because a tool block has no
    /// role label to fall back on. The row is the label.
    #[test]
    fn monochrome_still_separates_a_done_call_from_a_failed_one() {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Mono, Depth::TrueColor, set);
            let args = Some(r#"{"path":"a.rs"}"#);
            let done = plain(
                &block(
                    &call("read", args, ToolStatus::Done("ok".into())),
                    false,
                    60,
                    theme,
                )[0],
            );
            let failed = plain(
                &block(
                    &call("read", args, ToolStatus::Done("tool `read` trapped".into())),
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

    /// The seam #156 replaces: a result that is a diff stops being plain text.
    #[test]
    fn a_result_that_is_a_diff_renders_as_one_even_when_the_call_failed() {
        let theme = Theme::new(Mode::Dark, Depth::TrueColor, GlyphSet::Unicode);
        let patch = "@@ -1,2 +1,2 @@\n context\n-gone\n+new";
        let rendered: Vec<String> = block(
            &opened(call(
                "git",
                Some(r#"{"op":"diff"}"#),
                ToolStatus::Done(patch.into()),
            )),
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

        // The colliding case: a failed result is drawn in `removed()`, which is
        // the role a `-` line wants. The diff wins, and the failure is still
        // said on the head row.
        let failed = opened(call(
            "git",
            Some("{}"),
            ToolStatus::Done(format!("tool `git` error: {patch}")),
        ));
        let head = plain(&block(&failed, false, 60, theme)[0]);
        assert!(head.contains(theme.glyph(Glyph::ToolFailed)), "{head:?}");
    }

    /// A selection nobody can see is not a selection.
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

    /// What the scroll is told, so it has to agree with what was drawn.
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

    /// The gutter and the indent are part of the row here too.
    #[test]
    fn an_expanded_block_stays_inside_the_pane() {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let theme = Theme::new(Mode::Dark, Depth::TrueColor, set);
            let entry = opened(call(
                "grep",
                Some(r#"{"pattern":"a very long pattern that will not fit on one row at all"}"#),
                ToolStatus::Done("日本語のテキストです ".repeat(4)),
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
}
