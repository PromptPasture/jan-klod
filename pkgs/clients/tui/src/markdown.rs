//! Assistant text as Markdown, in greys. Emphasis uses modifiers, not accents, so it survives `Mode::Mono`.
//!
//! Text is still streaming, so an unterminated fence is normal: everything from
//! the fence on is shown verbatim. Wrapping happens after styling, so bold
//! survives a line break.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{Glyph, Theme};
use crate::wrap::width;

/// The byte offset of a fence that never closes. Scans lines, since a parser would auto-close it.
fn unterminated_fence(text: &str) -> Option<usize> {
    let mut open_at = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if fence {
            open_at = if open_at.is_some() {
                None
            } else {
                Some(offset)
            };
        }
        offset += line.len();
    }
    open_at
}

/// Lays styled spans across lines of at most `max` cells, breaking between words. `prefix` repeats on each line.
fn flow(spans: &[(String, Style)], max: usize, prefix: &(String, Style)) -> Vec<Line<'static>> {
    let inner = max.saturating_sub(width(&prefix.0)).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0;

    for (text, style) in spans {
        for word in text.split_whitespace() {
            let w = width(word);
            let space = usize::from(used > 0);
            if used + space + w > inner && used > 0 {
                let mut row = vec![Span::styled(prefix.0.clone(), prefix.1)];
                row.append(&mut current);
                lines.push(Line::from(row));
                used = 0;
            }
            if used > 0 {
                current.push(Span::styled(" ".to_string(), *style));
                used += 1;
            }
            current.push(Span::styled(word.to_string(), *style));
            used += w;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        let mut row = vec![Span::styled(prefix.0.clone(), prefix.1)];
        row.append(&mut current);
        lines.push(Line::from(row));
    }
    lines
}

/// Render `text` as Markdown, wrapped to `max` cells.
#[must_use]
pub fn render(text: &str, max: usize, theme: Theme) -> Vec<Line<'static>> {
    unterminated_fence(text).map_or_else(
        || render_closed(text, max, theme),
        |at| {
            let mut lines = render_closed(&text[..at], max, theme);
            lines.extend(verbatim(
                &text[at..],
                max,
                "",
                Style::default().fg(theme.body()),
            ));
            lines
        },
    )
}

/// Emits `text` wrapped in one style, with an `indent` prefix. The indent survives `Mode::Mono`, unlike the code surface.
pub(crate) fn verbatim(text: &str, max: usize, indent: &str, style: Style) -> Vec<Line<'static>> {
    let inner = max.saturating_sub(width(indent)).max(1);
    text.lines()
        .flat_map(|line| crate::wrap::wrap(line, inner))
        .map(|row| Line::from(Span::styled(format!("{indent}{row}"), style)))
        .collect()
}

#[allow(clippy::too_many_lines)]
fn render_closed(text: &str, max: usize, theme: Theme) -> Vec<Line<'static>> {
    let body = Style::default().fg(theme.body());
    let secondary = Style::default().fg(theme.secondary());
    let muted = Style::default().fg(theme.muted());
    let code = Style::default().fg(theme.body()).bg(theme.code_surface());
    let none = (String::new(), Style::default());

    let mut out: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<(String, Style)> = Vec::new();
    let mut style = body;
    let mut prefix = none.clone();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote_depth = 0usize;
    let mut in_code: Option<String> = None;
    let mut link: Option<String> = None;

    for event in Parser::new_ext(text, Options::empty()) {
        match event {
            Event::Start(Tag::Heading { .. }) => style = body.add_modifier(Modifier::BOLD),
            Event::Start(Tag::Emphasis) => style = style.add_modifier(Modifier::ITALIC),
            Event::Start(Tag::Strong) => style = style.add_modifier(Modifier::BOLD),
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.to_string()),
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::Start(Tag::CodeBlock(_)) => in_code = Some(String::new()),
            Event::Start(Tag::Item) => {
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                prefix = (marker, muted);
            }

            Event::End(TagEnd::Emphasis) => style = style.remove_modifier(Modifier::ITALIC),
            Event::End(TagEnd::Strong) => style = style.remove_modifier(Modifier::BOLD),
            Event::End(TagEnd::Link) => {
                if let Some(url) = link.take() {
                    spans.push((format!(" ({url})"), muted));
                }
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(source) = in_code.take() {
                    out.extend(verbatim(source.trim_end_matches('\n'), max, "  ", code));
                }
            }
            Event::End(TagEnd::Heading(_) | TagEnd::Paragraph | TagEnd::Item) => {
                let indent = if quote_depth > 0 {
                    (
                        format!("{} ", theme.glyph(Glyph::MessageGutter)).repeat(quote_depth),
                        secondary,
                    )
                } else {
                    prefix.clone()
                };
                let flowed = if quote_depth > 0 { secondary } else { style };
                let restyled: Vec<(String, Style)> = std::mem::take(&mut spans)
                    .into_iter()
                    .map(|(t, s)| {
                        if quote_depth > 0 && s.bg.is_none() {
                            (t, flowed)
                        } else {
                            (t, s)
                        }
                    })
                    .collect();
                out.extend(flow(&restyled, max, &indent));
                prefix = none.clone();
                style = body;
            }

            Event::Text(t) => match &mut in_code {
                Some(buf) => buf.push_str(&t),
                None => spans.push((t.to_string(), style)),
            },
            Event::Code(t) => spans.push((t.to_string(), code)),
            Event::SoftBreak | Event::HardBreak => spans.push((" ".to_string(), style)),
            Event::Rule => out.push(Line::from(Span::styled(
                "─".repeat(max.min(40)),
                Style::default().fg(theme.border_idle()),
            ))),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{render, unterminated_fence};
    use crate::theme::{Depth, GlyphSet, Mode, Theme};
    use ratatui::text::Line;

    fn theme(mode: Mode) -> Theme {
        Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode)
    }

    fn plain(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_closed_fence_loses_its_markers_and_sits_on_the_code_surface() {
        let md = "before\n\n```rust\nfn main() {}\n```\n\nafter";
        let lines = render(md, 40, theme(Mode::Dark));
        let text = plain(&lines);

        assert!(!text.contains("```"), "fence markers survived: {text:?}");
        assert!(text.contains("fn main() {}"), "the body is gone: {text:?}");
        assert!(text.contains("before") && text.contains("after"));

        let surface = theme(Mode::Dark).code_surface();
        let on_surface: Vec<&Line<'_>> = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.bg == Some(surface)))
            .collect();
        assert_eq!(
            on_surface.len(),
            1,
            "exactly the code row is on the surface"
        );
        assert!(plain(&[on_surface[0].clone()]).contains("fn main"));
    }

    /// Streaming rule: an open fence stays plain, since styling it would mean restyling it on close.
    #[test]
    fn an_unterminated_fence_stays_plain_with_its_backticks_shown() {
        let md = "intro\n\n```rust\nfn half_arriv";
        let lines = render(md, 40, theme(Mode::Dark));
        let text = plain(&lines);

        assert!(
            text.contains("```rust"),
            "the backticks must show: {text:?}"
        );
        assert!(text.contains("fn half_arriv"));
        assert!(
            text.contains("intro"),
            "text before the fence still renders"
        );

        let surface = theme(Mode::Dark).code_surface();
        assert!(
            lines
                .iter()
                .all(|l| l.spans.iter().all(|s| s.style.bg != Some(surface))),
            "an open fence was styled as a code block, so closing it will restyle"
        );
    }

    #[test]
    fn the_fence_scanner_counts_pairs() {
        assert_eq!(unterminated_fence("no fences here"), None);
        assert_eq!(unterminated_fence("```\nx\n```\n"), None);
        assert!(unterminated_fence("```\nx\n").is_some());
        assert_eq!(
            unterminated_fence("```\na\n```\n```\nb\n"),
            Some(10),
            "the second opener, not the first"
        );
    }

    /// With colour gone, the code indent is the only cue left.
    #[test]
    fn monochrome_still_tells_code_from_prose() {
        let md = "a sentence\n\n```\nsome_code()\n```";
        let lines = render(md, 40, theme(Mode::Mono));
        let rows: Vec<String> = plain(&lines).lines().map(str::to_string).collect();

        let code = rows
            .iter()
            .find(|r| r.contains("some_code()"))
            .expect("the code row");
        let prose = rows
            .iter()
            .find(|r| r.contains("a sentence"))
            .expect("the prose row");
        assert!(
            code.starts_with("  ") && !prose.starts_with("  "),
            "with colour gone the indent is the only thing left: {code:?} vs {prose:?}"
        );
    }

    #[test]
    fn markdown_uses_no_accent_colour() {
        let t = theme(Mode::Dark);
        let accents = [t.focus(), t.added(), t.removed(), t.warning()];
        let md = "# Heading\n\nSome **bold** and *italic* and `code`.\n\n- one\n- two\n\n> quoted\n\n[text](http://example.com)";
        for line in render(md, 40, t) {
            for span in &line.spans {
                if let Some(fg) = span.style.fg {
                    assert!(
                        !accents.contains(&fg),
                        "{:?} took an accent; the four accents each mean one thing \
                         and document structure is not one of them",
                        span.content
                    );
                }
            }
        }
    }

    /// No fence is syntax-highlighted. A future highlighting slice must change this test on purpose.
    #[test]
    fn no_fence_carries_syntax_colour_tagged_or_not() {
        let t = theme(Mode::Dark);
        for md in [
            "```rust\nfn main() { let x: u8 = 1; }\n```",
            "```\nfn main() { let x: u8 = 1; }\n```",
        ] {
            let lines = render(md, 60, t);
            let colours: std::collections::BTreeSet<String> = lines
                .iter()
                .flat_map(|l| &l.spans)
                .filter(|s| !s.content.trim().is_empty())
                .map(|s| format!("{:?}", s.style.fg))
                .collect();
            assert_eq!(
                colours.len(),
                1,
                "a code block took more than one foreground, which is syntax \
                 colour by another name: {colours:?}"
            );
        }
    }

    #[test]
    fn structure_survives_the_round_trip() {
        let t = theme(Mode::Dark);
        let md = "# Title\n\n- alpha\n- beta\n\n1. first\n2. second\n\n> a quote\n\n[jan](http://example.com)";
        let text = plain(&render(md, 60, t));
        assert!(text.contains("Title"));
        assert!(text.contains("• alpha") && text.contains("• beta"));
        assert!(text.contains("1. first") && text.contains("2. second"));
        assert!(text.contains("a quote"));
        assert!(
            text.contains("jan") && text.contains("(http://example.com)"),
            "a link is its text plus its URL, not an OSC 8 escape: {text:?}"
        );
    }
}
