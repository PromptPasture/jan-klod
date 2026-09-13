//! A unified diff, whose signs survive monochrome (#156).
//!
//! # Nothing here computes a diff
//!
//! `tool-git op=diff` runs git and returns git's stdout (already a unified diff).
//! This parses and renders it; see `src/tui/README.md` for measurements.
//!
//! # The sign is a character, not a colour
//!
//! Under [`Mode::Mono`](crate::theme::Mode::Mono), colour is irrelevant.
//! `+` and `-` are **real characters in column 0**, not styled glyphs.
//! Number column width from largest line number in whole diff (not per-hunk),
//! so signs stay aligned. Unified, not split (split needs 100 cols, client reads at 60).
//!
//! # Not a diff is common; wrong guess costs nothing
//!
//! [`render`] returns `None` for non-diffs; caller falls back to plain text.
//! Tool output isn't a contract (`tool-git` uses `(no output)`, `tool-edit` uses prose).
//! Exception: truncation marker (from `guest_fs::truncate`) is recognized and shown.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Theme;
use crate::wrap::{hard_wrap, width};

/// What the guests append when output hits the cap. See the module docs: this
/// is a convention shared with `src/extensions/guest-fs`, not a wire contract.
const TRUNCATION: &str = "…[truncated:";

/// One parsed line of a diff.
#[derive(Debug, PartialEq, Eq)]
enum Row<'a> {
    /// `diff --git`, `index`, `---`, `+++`, and the mode/rename lines.
    File(&'a str),
    /// `@@ -a,b +c,d @@ section`.
    Hunk(&'a str),
    /// A body line: its sign, the line number on each side, and the text.
    Body {
        /// `' '`, `'+'` or `'-'` — and it reaches column 0 unchanged.
        sign: char,
        /// Line number on the old side, absent for an addition.
        old: Option<usize>,
        /// Line number on the new side, absent for a removal.
        new: Option<usize>,
        /// The line itself, with the sign already stripped.
        text: &'a str,
    },
    /// `\ No newline at end of file`, or the guests' truncation marker: true of
    /// the diff, but not a line of the file.
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

/// Parse `text` as a unified diff, returning `None` if any line doesn't fit.
/// Strict: lenient parsing renders half a log as a diff (worse than failing).
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
            // A header after a hunk is the next file, so the counters end here
            // rather than carrying into it.
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
            // Empty line = context line with trailing space stripped (mail, paste, tool trim).
            // Treating as prose rejects most diffs that passed through anything.
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

    // One hunk is the whole bar: without it there is nothing a diff view shows
    // that plain text does not.
    rows.iter()
        .any(|row| matches!(row, Row::Hunk(_)))
        .then_some(rows)
}

/// The number column, right-aligned, or blanks where that side has no line.
fn number(value: Option<usize>, cells: usize) -> String {
    value.map_or_else(|| " ".repeat(cells), |n| format!("{n:>cells$}"))
}

/// The `(added, removed)` line counts of `text`, if it parses as a unified diff.
///
/// `None` if it does not, which is the sidebar's signal (#104's CHANGED
/// section) to show the path with no counts rather than guessing `+0 -0`.
/// Shares [`parse`] with [`render`] rather than re-deriving the count from the
/// rendered lines, so the two can never disagree about what counts as a diff.
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

/// Render `text` as a unified diff, wrapped to `max` cells — or `None` if it is
/// not one, which is the caller's signal to show it as plain text.
#[must_use]
pub fn render(text: &str, max: usize, theme: Theme) -> Option<Vec<Line<'static>>> {
    let rows = parse(text)?;
    let surface = theme.code_surface();

    // One column width for whole diff (from largest line number);
    // changing per-hunk would break sign alignment—this layout's purpose.
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
            // One arm, deliberately: a file header, a hunk header and a note
            // are all statements *about* the diff rather than lines of the
            // file, they carry no line number, and `muted()` is what the issue
            // asks for on the hunk header. Splitting them to look thorough
            // would be three ways to spell the same decision.
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
        // A wrapped continuation gets blanks where the sign and the numbers
        // were: repeating them would claim the same line was added twice.
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

    /// Acceptance (#104's CHANGED section): a parsed diff's counts, and a
    /// non-diff's `None` rather than a guessed `+0 -0`.
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

    /// Acceptance: a two-hunk diff, signs as characters in column 0.
    #[test]
    fn both_hunks_render_and_the_sign_is_a_character_in_column_zero() {
        let rendered = plain(&render(TWO_HUNKS, 60, theme(Mode::Dark)).expect("a diff"));

        let hunks = rendered.iter().filter(|r| r.starts_with("@@")).count();
        assert_eq!(hunks, 2, "both hunk headers: {rendered:?}");

        // Not simply `starts_with('+')`: `+++ b/…` and `--- a/…` start with a
        // sign and are headers, which is the mistake a sign-first parser makes
        // and the reason `is_file_header` is consulted first.
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

        // The `---`/`+++` headers must not be read as a removal and an
        // addition, which is exactly what a sign-first parser does wrong.
        assert!(
            rendered.iter().any(|r| r.starts_with("--- a/src/main.rs")),
            "the file header was eaten: {rendered:?}"
        );
        assert!(
            !removed.iter().any(|r| r.contains("a/src/main.rs")),
            "a file header rendered as a removed line: {removed:?}"
        );
    }

    /// Both sides are numbered, from the hunk header rather than from zero.
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
        // The second hunk reaches line 22, so the column is two digits wide —
        // and it is two digits in the *first* hunk too, because one width for
        // the whole diff is what keeps the signs lining up.
        assert!(
            body.iter().any(|r| r.starts_with("+   21 added")),
            "the second hunk is not numbered from its header: {body:?}"
        );
    }

    /// Acceptance: the same render under `Mono`, differing in nothing but style.
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

    /// Acceptance: diff-shaped but unparseable renders as plain text.
    #[test]
    fn anything_that_is_not_a_diff_is_refused_rather_than_guessed_at() {
        for content in [
            // `tool-git`'s own answer for "nothing changed".
            "(no output)",
            // `tool-edit`'s answer, which begins with neither sign nor hunk.
            "replace applied to src/main.rs (3 line(s)).",
            // Diff-shaped and not a diff: signs, no hunk header.
            "--- a/x\n+++ b/x\n-gone\n+new",
            // A hunk header that does not parse.
            "@@ not really @@\n-gone\n+new",
            // Headers and nothing else, which git really does emit for a mode
            // change — and which every line of parses. Without a hunk there is
            // nothing a diff view shows that plain text does not, and this is
            // the only case where that rule is what refuses it.
            "diff --git a/x b/x\nold mode 100644\nnew mode 100755",
            // Prose after a hunk, which is where a lenient parser would render
            // somebody's log as a diff and be convincing about it.
            "@@ -1,2 +1,2 @@\n context\nthen a sentence with no sign at all",
            "",
        ] {
            assert!(
                render(content, 60, theme(Mode::Dark)).is_none(),
                "{content:?} was rendered as a diff"
            );
        }
    }

    /// The realistic malformed diff: the guests' own byte cap cut it.
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

        // `\ No newline at end of file` is true of the diff, not a line of it.
        let no_newline = "@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file";
        let rows = parse(no_newline).expect("a diff");
        assert_eq!(rows.len(), 4);
    }

    /// Alignment is the content here, so a long line breaks without losing a
    /// cell — and the continuation does not repeat the sign.
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
