//! Parses and renders a unified diff. Signs are real characters in column 0, so they survive monochrome.
//!
//! Nothing here computes a diff; `tool-git` supplies one. Non-diffs return
//! `None` and the caller shows plain text.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::wrap::{hard_wrap, width};

/// Appended by the guests when output hits its cap. A shared convention, not a wire contract.
const TRUNCATION: &str = "…[truncated:";

/// One parsed line of a diff.
#[derive(Debug, PartialEq, Eq)]
enum Row<'a> {
    File(&'a str),
    Hunk(&'a str),
    /// A body line; a side it does not appear on has no line number.
    Body {
        sign: char,
        old: Option<usize>,
        new: Option<usize>,
        text: &'a str,
    },
    /// `\ No newline at end of file`, or the truncation marker: not a line of the file.
    Note(&'a str),
}

/// The `-a,b +c,d` of a hunk header, or `None` if it is not one.
fn hunk_start(line: &str) -> Option<(usize, usize)> {
    let inner = line.strip_prefix("@@ ")?;
    let (ranges, _) = inner.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let first = |side: &str, mark: char| -> Option<usize> {
        let digits = side.strip_prefix(mark)?;
        // `,count` is optional: git omits it for a one-line range.
        digits
            .split_once(',')
            .map_or(digits, |(start, _)| start)
            .parse()
            .ok()
    };
    Some((first(old, '-')?, first(new, '+')?))
}

/// Whether a line is one of the per-file headers that open a diff.
fn is_file_header(line: &str) -> bool {
    [
        "diff --git ",
        "index ",
        "--- ",
        "+++ ",
        "old mode ",
        "new mode ",
        "deleted file mode ",
        "new file mode ",
        "similarity index ",
        "rename from ",
        "rename to ",
        "Binary files ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
        || line == "--- /dev/null"
        || line == "+++ /dev/null"
}

/// Parses `text` as a unified diff, or `None` if any line does not fit. Strict on purpose: a lenient parser would render half a log as a diff.
fn parse(text: &str) -> Option<Vec<Row<'_>>> {
    let mut rows = Vec::new();
    let mut counters: Option<(usize, usize)> = None;

    for line in text.lines() {
        if let Some((old, new)) = hunk_start(line) {
            counters = Some((old, new));
            rows.push(Row::Hunk(line));
            continue;
        }
        if is_file_header(line) {
            // A header starts the next file, so the previous hunk's counters end here.
            counters = None;
            rows.push(Row::File(line));
            continue;
        }
        if line.starts_with(TRUNCATION) {
            rows.push(Row::Note(line));
            continue;
        }
        let Some((old, new)) = counters.as_mut() else {
            // Outside a hunk, anything that is not a header is prose.
            return None;
        };
        let (sign, text) = match line.chars().next() {
            Some('+') => ('+', &line[1..]),
            Some('-') => ('-', &line[1..]),
            Some(' ') => (' ', &line[1..]),
            Some('\\') => {
                rows.push(Row::Note(line));
                continue;
            }
            // A context line whose trailing space was stripped (mail, paste, tool trim).
            None => (' ', ""),
            Some(_) => return None,
        };
        let (shown_old, shown_new) = match sign {
            '+' => (None, Some(*new)),
            '-' => (Some(*old), None),
            _ => (Some(*old), Some(*new)),
        };
        if sign != '+' {
            *old += 1;
        }
        if sign != '-' {
            *new += 1;
        }
        rows.push(Row::Body {
            sign,
            old: shown_old,
            new: shown_new,
            text,
        });
    }

    // Without a hunk, a diff view shows nothing that plain text does not.
    rows.iter()
        .any(|row| matches!(row, Row::Hunk(_)))
        .then_some(rows)
}

/// The number column, right-aligned, or blanks where that side has no line.
fn number(value: Option<usize>, cells: usize) -> String {
    value.map_or_else(|| " ".repeat(cells), |n| format!("{n:>cells$}"))
}

/// The `(added, removed)` line counts of `text`, or `None` if it is not a diff.
///
/// Shares [`parse`] with [`render`], so the two cannot disagree about what is a diff.
#[must_use]
pub fn counts(text: &str) -> Option<(usize, usize)> {
    let rows = parse(text)?;
    let mut added = 0usize;
    let mut removed = 0usize;
    for row in &rows {
        if let Row::Body { sign, .. } = row {
            match sign {
                '+' => added += 1,
                '-' => removed += 1,
                _ => {}
            }
        }
    }
    Some((added, removed))
}

/// Renders `text` as a unified diff wrapped to `max` cells, or `None` if it is not one.
#[must_use]
pub fn render(text: &str, max: usize, theme: Theme) -> Option<Vec<Line<'static>>> {
    let rows = parse(text)?;
    let surface = theme.code_surface();

    // One width for the whole diff, so the signs line up across hunks.
    let digits = rows
        .iter()
        .filter_map(|row| match row {
            Row::Body { old, new, .. } => Some(old.unwrap_or(0).max(new.unwrap_or(0))),
            _ => None,
        })
        .max()
        .map_or(1, |largest| width(&largest.to_string()).max(1));
    let mut lines = Vec::new();
    for row in rows {
        let (prefix, body, style) = match row {
            // Statements about the diff, not lines of the file: no numbers, one style.
            Row::File(text) | Row::Hunk(text) | Row::Note(text) => (
                String::new(),
                text.to_string(),
                Style::default().fg(theme.muted()).bg(surface),
            ),
            Row::Body {
                sign,
                old,
                new,
                text,
            } => (
                format!("{sign}{} {} ", number(old, digits), number(new, digits)),
                text.to_string(),
                Style::default()
                    .fg(match sign {
                        '+' => theme.added(),
                        '-' => theme.removed(),
                        _ => theme.secondary(),
                    })
                    .bg(surface),
            ),
        };
        // Continuations get blanks, not the sign: repeating it would claim a second change.
        let room = max.saturating_sub(width(&prefix)).max(1);
        for (i, part) in hard_wrap(&body, room).into_iter().enumerate() {
            let lead = if i == 0 {
                prefix.clone()
            } else {
                " ".repeat(width(&prefix))
            };
            lines.push(Line::from(Span::styled(format!("{lead}{part}"), style)));
        }
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::{counts, parse, render};

    /// A non-diff gives `None`, not a guessed `+0 -0`.
    #[test]
    fn counts_the_added_and_removed_lines_or_says_it_could_not_parse() {
        assert_eq!(counts(TWO_HUNKS), Some((2, 1)));
        assert_eq!(counts("replace applied to src/main.rs (3 line(s))."), None);
        assert_eq!(counts(""), None);
    }
    use crate::theme::{Depth, GlyphSet, Mode, Theme};
    use crate::wrap::width;

    /// Two hunks, both sides numbered, with a `+` and a `-` in each.
    const TWO_HUNKS: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,4 @@ fn main
 use std::io;
-let a = 1;
+let a = 2;
 use std::fmt;
@@ -20,3 +20,4 @@
 context
+added
 tail";

    fn theme(mode: Mode) -> Theme {
        Theme::new(mode, Depth::TrueColor, GlyphSet::Unicode)
    }

    fn plain(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    /// Signs are characters in column 0.
    #[test]
    fn both_hunks_render_and_the_sign_is_a_character_in_column_zero() {
        let rendered = plain(&render(TWO_HUNKS, 60, theme(Mode::Dark)).expect("a diff"));

        let hunks = rendered.iter().filter(|r| r.starts_with("@@")).count();
        assert_eq!(hunks, 2, "both hunk headers: {rendered:?}");

        // `+++`/`---` headers also start with a sign, so they are not counted here.
        let added: Vec<&String> = rendered
            .iter()
            .filter(|r| r.starts_with('+') && !r.starts_with("+++"))
            .collect();
        let removed: Vec<&String> = rendered
            .iter()
            .filter(|r| r.starts_with('-') && !r.starts_with("---"))
            .collect();
        assert_eq!(added.len(), 2, "`+` is not in column 0: {rendered:?}");
        assert_eq!(removed.len(), 1, "`-` is not in column 0: {rendered:?}");
        assert!(
            added.iter().any(|r| r.contains("let a = 2;")),
            "{rendered:?}"
        );

        assert!(
            rendered.iter().any(|r| r.starts_with("--- a/src/main.rs")),
            "the file header was eaten: {rendered:?}"
        );
        assert!(
            !removed.iter().any(|r| r.contains("a/src/main.rs")),
            "a file header rendered as a removed line: {removed:?}"
        );
    }

    /// Both sides are numbered from the hunk header.
    #[test]
    fn line_numbers_come_from_the_hunk_header_and_skip_the_side_that_has_none() {
        let rendered = plain(&render(TWO_HUNKS, 60, theme(Mode::Dark)).expect("a diff"));
        let body: Vec<&String> = rendered
            .iter()
            .filter(|r| !r.starts_with("@@") && !r.starts_with("diff") && !r.starts_with("index"))
            .collect();

        assert!(
            body.iter().any(|r| r.starts_with("  1  1 use std::io;")),
            "the first context line is not numbered 1/1: {body:?}"
        );
        assert!(
            body.iter().any(|r| r.starts_with("- 2    let a = 1;")),
            "a removal must number the old side only: {body:?}"
        );
        assert!(
            body.iter().any(|r| r.starts_with("+    2 let a = 2;")),
            "an addition must number the new side only: {body:?}"
        );
        // The second hunk reaches line 22, so the first hunk is padded to two digits too.
        assert!(
            body.iter().any(|r| r.starts_with("+   21 added")),
            "the second hunk is not numbered from its header: {body:?}"
        );
    }

    /// Mono changes the colours and not one character.
    #[test]
    fn monochrome_changes_the_colours_and_not_one_character() {
        let colour = render(TWO_HUNKS, 60, theme(Mode::Dark)).expect("a diff");
        let mono = render(TWO_HUNKS, 60, theme(Mode::Mono)).expect("a diff");
        assert_eq!(
            plain(&colour),
            plain(&mono),
            "the text of a diff must not depend on having colour"
        );
        assert_ne!(
            colour[5].spans[0].style, mono[5].spans[0].style,
            "this test only means something if the styles did differ"
        );
    }

    /// Diff-shaped but unparseable content renders as plain text.
    #[test]
    fn anything_that_is_not_a_diff_is_refused_rather_than_guessed_at() {
        for content in [
            // `tool-git`'s answer for no changes.
            "(no output)",
            // `tool-edit`'s prose answer.
            "replace applied to src/main.rs (3 line(s)).",
            // Signs but no hunk header.
            "--- a/x\n+++ b/x\n-gone\n+new",
            // A hunk header that does not parse.
            "@@ not really @@\n-gone\n+new",
            // Headers only, as git emits for a mode change: no hunk, so nothing a diff view adds.
            "diff --git a/x b/x\nold mode 100644\nnew mode 100755",
            // Prose after a hunk: a lenient parser would render a log as a diff.
            "@@ -1,2 +1,2 @@\n context\nthen a sentence with no sign at all",
            "",
        ] {
            assert!(
                render(content, 60, theme(Mode::Dark)).is_none(),
                "{content:?} was rendered as a diff"
            );
        }
    }

    /// The guests' output cap cut the diff. It still renders, and says so.
    #[test]
    fn a_diff_the_output_cap_cut_still_renders_and_says_it_was_cut() {
        let cut = "@@ -1,9 +1,9 @@\n context\n-gone\n+new\n…[truncated: 4096 bytes omitted]";
        let rendered =
            plain(&render(cut, 60, theme(Mode::Dark)).expect("a cut diff still renders"));
        assert!(
            rendered.last().is_some_and(|r| r.contains("truncated")),
            "the cut was not shown: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|r| r.starts_with("+  2 new")),
            "the part that arrived was not rendered: {rendered:?}"
        );

        // The no-newline marker is a note, not a file line.
        let no_newline = "@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file";
        let rows = parse(no_newline).expect("a diff");
        assert_eq!(rows.len(), 4);
    }

    /// A long line wraps without claiming to be a second change.
    #[test]
    fn a_long_line_wraps_without_claiming_to_be_a_second_change() {
        let long = format!("@@ -1 +1 @@\n+    let x = 1;{}// aligned", " ".repeat(30));
        let rendered = plain(&render(&long, 24, theme(Mode::Dark)).expect("a diff"));
        for row in &rendered {
            assert!(width(row) <= 24, "{row:?} is {} cells", width(row));
        }
        let signs = rendered.iter().filter(|r| r.starts_with('+')).count();
        assert_eq!(signs, 1, "a wrapped line claimed a second addition");
        let joined: String = rendered[1..].concat();
        assert!(
            joined.contains(&" ".repeat(30)),
            "the padding this diff is *about* was collapsed: {rendered:?}"
        );
    }
}
