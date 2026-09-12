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

use crate::app::{Entry, ToolStatus, Who};
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

/// Render one entry into lines, wrapped to `width` cells including the gutter.
#[must_use]
pub fn block(entry: &Entry, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (who, text) = match entry {
        Entry::Message { who, text } => (*who, text.clone()),
        // Minimal for now, and deliberately: #155 owns the collapsed line that
        // names what the call *did* rather than its JSON, the `Ctrl+O` toggle
        // and the expanded form. This keeps a tool block visible and paired in
        // the meantime rather than leaving it unrendered, which would make
        // #154's model untestable through the thing that draws it.
        Entry::Tool(tool) => {
            let mark = match &tool.status {
                ToolStatus::Running => theme.glyph(Glyph::Collapsed).to_string(),
                ToolStatus::Done(_) => theme.glyph(Glyph::ToolDone).to_string(),
                ToolStatus::Interrupted => theme.glyph(Glyph::ToolFailed).to_string(),
            };
            let suffix = match &tool.status {
                ToolStatus::Running => String::new(),
                ToolStatus::Done(content) => format!(" {content}"),
                ToolStatus::Interrupted => " interrupted".to_string(),
            };
            (Who::Status, format!("{mark} {}{suffix}", tool.name))
        }
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
pub fn transcript(entries: &[Entry], width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(String::new()));
        }
        lines.extend(block(entry, width, theme));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{block, transcript};
    use crate::app::{Entry, Who};
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
            let lines = block(&entry(who, "a message"), 40, theme);
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
            let lines = block(&entry(who, "a message"), 40, theme);
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
            .map(|who| plain(&block(&entry(who, "x"), 40, theme)[0]))
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
            transcript(&[], 40, theme).is_empty(),
            "no entries, no lines"
        );
    }
}
