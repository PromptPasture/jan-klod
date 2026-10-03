//! `tool-edit` — hash-anchored line edits routed through `host-fs`.
//!
//! Exposes `edit` tool with operations:
//! - `view` → file as `anchor|lineno|text` lines
//! - `replace` → replace lines `start..=end` with contents (empty deletes)
//! - `insert` → insert contents before/after anchored line
//!
//! An **anchor** is `fnv1a32(lineno \0 text)`. If file changed, no anchor resolves;
//! stale edits are rejected without write. Rejections return recovery text (re-view, retry).
//! The patch logic is pure Rust (unit-tested natively); Component-Model glue only for wasm32.

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod edit {

    /// FNV-1a 32-bit offset basis.
    const FNV_OFFSET: u32 = 0x811c_9dc5;
    /// FNV-1a 32-bit prime.
    const FNV_PRIME: u32 = 0x0100_0193;

    /// FNV-1a over `bytes`. Chosen because it needs no dependency and the anchor
    /// is a change detector, not a security primitive — a collision costs a
    /// rejected edit (both candidates are reported ambiguous), never a bad write.
    fn fnv1a32(bytes: &[u8]) -> u32 {
        bytes.iter().fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(FNV_PRIME)
        })
    }

    /// Anchor for the 1-based line `lineno` holding `text`.
    /// Hashing both makes the anchor fail closed: moved or changed lines differ.
    pub fn anchor(lineno: usize, text: &str) -> String {
        let mut buf = lineno.to_string().into_bytes();
        buf.push(0);
        buf.extend_from_slice(text.as_bytes());
        format!("{:08x}", fnv1a32(&buf))
    }

    /// Split `content` into lines and whether it ended with a newline.
    fn lines_of(content: &str) -> (Vec<String>, bool) {
        content.strip_suffix('\n').map_or_else(
            || (content.split('\n').map(str::to_string).collect(), false),
            |body| (body.split('\n').map(str::to_string).collect(), true),
        )
    }

    /// Re-join `lines`, restoring the original trailing newline.
    fn join_lines(lines: &[String], trailing_newline: bool) -> String {
        let mut out = lines.join("\n");
        if trailing_newline {
            out.push('\n');
        }
        out
    }

    /// Split replacement `contents` into lines, dropping trailing newline.
    /// The re-join supplies the separator between blocks.
    fn contents_lines(contents: &str) -> Vec<String> {
        if contents.is_empty() {
            return Vec::new();
        }
        contents
            .strip_suffix('\n')
            .unwrap_or(contents)
            .split('\n')
            .map(str::to_string)
            .collect()
    }

    /// Resolve `target` to its 0-based line index.
    /// # Errors
    /// Anchor misses (file changed) or is ambiguous (collision). Both recoverable by re-viewing.
    fn find_anchor(lines: &[String], target: &str) -> Result<usize, String> {
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(index, line)| anchor(index + 1, line) == *target)
            .map(|(index, _)| index)
            .collect();
        match hits.as_slice() {
            [only] => Ok(*only),
            [] => Err(format!(
                "anchor `{target}` matches no line — the file changed since you viewed it. \
                 Run op=view again and retry with fresh anchors."
            )),
            _ => Err(format!(
                "anchor `{target}` is ambiguous ({} matching lines). Run op=view again.",
                hits.len()
            )),
        }
    }

    /// Render `content` as one `anchor|lineno|text` line per source line.
    pub fn render_view(content: &str) -> String {
        let (lines, _) = lines_of(content);
        lines
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}|{}|{line}", anchor(index + 1, line), index + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replace lines `start..=end` (`end` defaults to `start`) with `contents`.
    /// Empty `contents` deletes the span. Returns new text and line count.
    /// # Errors
    /// Anchor does not resolve or `end` precedes `start`. Nothing is written.
    pub fn replace(
        content: &str,
        start: &str,
        end: Option<&str>,
        contents: &str,
    ) -> Result<(String, usize), String> {
        let (mut lines, trailing_newline) = lines_of(content);
        let first = find_anchor(&lines, start)?;
        let last = match end {
            Some(anchor) => find_anchor(&lines, anchor)?,
            None => first,
        };
        if last < first {
            return Err(format!(
                "end anchor `{}` precedes start anchor `{start}` — swap them.",
                end.unwrap_or(start)
            ));
        }
        let replaced = last - first + 1;
        lines.splice(first..=last, contents_lines(contents));
        Ok((join_lines(&lines, trailing_newline), replaced))
    }

    /// Insert `contents` before or after the anchored line. Returns new file text and line count.
    /// # Errors
    /// Anchor does not resolve or `contents` is empty
    /// (an insert that changes nothing is a mistake, not a no-op).
    pub fn insert(
        content: &str,
        target: &str,
        before: bool,
        contents: &str,
    ) -> Result<(String, usize), String> {
        let (mut lines, trailing_newline) = lines_of(content);
        let index = find_anchor(&lines, target)?;
        let block = contents_lines(contents);
        if block.is_empty() {
            return Err("`contents` is empty — nothing to insert.".to_string());
        }
        let at = if before { index } else { index + 1 };
        let inserted = block.len();
        lines.splice(at..at, block);
        Ok((join_lines(&lines, trailing_newline), inserted))
    }

    #[cfg(test)]
    mod tests {
        use super::{anchor, insert, render_view, replace};
        use guest_fs::{truncate, MAX_OUTPUT_BYTES};

        /// Anchor of line `lineno` as `view` renders it.
        fn anchor_of(content: &str, lineno: usize) -> String {
            anchor(lineno, content.split('\n').nth(lineno - 1).unwrap())
        }

        #[test]
        fn view_anchors_every_line() {
            let view = render_view("alpha\nbeta\n");
            let lines: Vec<&str> = view.lines().collect();
            assert_eq!(lines.len(), 2);
            assert!(lines[0].ends_with("|1|alpha"), "got {}", lines[0]);
            assert!(lines[1].ends_with("|2|beta"), "got {}", lines[1]);
        }

        #[test]
        fn identical_lines_get_distinct_anchors() {
            // Position + text are hashed, so duplicates resolve uniquely.
            let view = render_view("}\n}\n");
            let lines: Vec<&str> = view.lines().collect();
            assert_ne!(
                lines[0].split('|').next(),
                lines[1].split('|').next(),
                "duplicate lines must not share an anchor"
            );
        }

        #[test]
        fn replace_swaps_a_single_line() {
            let content = "alpha\nbeta\ngamma\n";
            let (out, replaced) = replace(content, &anchor_of(content, 2), None, "BETA").unwrap();
            assert_eq!(out, "alpha\nBETA\ngamma\n");
            assert_eq!(replaced, 1);
        }

        #[test]
        fn replace_spans_start_to_end_inclusive() {
            let content = "a\nb\nc\nd\n";
            let (out, replaced) = replace(
                content,
                &anchor_of(content, 2),
                Some(&anchor_of(content, 3)),
                "X\nY\nZ",
            )
            .unwrap();
            assert_eq!(out, "a\nX\nY\nZ\nd\n");
            assert_eq!(replaced, 2);
        }

        #[test]
        fn replace_with_empty_contents_deletes_the_span() {
            let content = "a\nb\nc\n";
            let (out, _) = replace(content, &anchor_of(content, 2), None, "").unwrap();
            assert_eq!(out, "a\nc\n");
        }

        #[test]
        fn stale_anchor_is_rejected_without_a_write() {
            let content = "a\nb\nc\n";
            let stale = anchor_of(content, 2);
            // The file moved on: line 2 is now at line 3.
            let changed = "header\na\nb\nc\n";
            let err = replace(changed, &stale, None, "B").unwrap_err();
            assert!(err.contains("matches no line"), "got {err}");
            assert!(
                err.contains("op=view"),
                "rejection must say how to recover: {err}"
            );
        }

        #[test]
        fn end_before_start_is_rejected() {
            let content = "a\nb\nc\n";
            let err = replace(
                content,
                &anchor_of(content, 3),
                Some(&anchor_of(content, 1)),
                "X",
            )
            .unwrap_err();
            assert!(err.contains("precedes"), "got {err}");
        }

        #[test]
        fn insert_places_contents_after_the_anchor() {
            let content = "a\nc\n";
            let (out, inserted) = insert(content, &anchor_of(content, 1), false, "b").unwrap();
            assert_eq!(out, "a\nb\nc\n");
            assert_eq!(inserted, 1);
        }

        #[test]
        fn insert_before_reaches_the_first_line() {
            let content = "a\nb\n";
            let (out, _) = insert(content, &anchor_of(content, 1), true, "header").unwrap();
            assert_eq!(out, "header\na\nb\n");
        }

        #[test]
        fn insert_with_empty_contents_is_rejected() {
            let content = "a\n";
            let err = insert(content, &anchor_of(content, 1), false, "").unwrap_err();
            assert!(err.contains("nothing to insert"), "got {err}");
        }

        #[test]
        fn a_file_without_a_trailing_newline_keeps_none() {
            let content = "a\nb";
            let (out, _) = replace(content, &anchor_of(content, 1), None, "A").unwrap();
            assert_eq!(out, "A\nb");
        }

        #[test]
        fn output_over_cap_is_truncated_with_marker() {
            let big = "x".repeat(MAX_OUTPUT_BYTES + 100);
            let result = truncate(big);
            assert!(
                result.contains("…[truncated:"),
                "truncation marker must be present"
            );
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::edit;

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod bindings {
        wit_bindgen::generate!({ world: "tool-world", path: "../../../wit" });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_fs;

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            Ok(())
        }
        fn start() -> Result<(), String> {
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    /// A rejected edit: no write happened, and the model is told how to recover.
    fn rejected(reason: &str) -> String {
        format!("REJECTED (nothing was written): {reason}")
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "edit".to_string(),
                description: "Edit part of a workspace file without rewriting it. \
                    Always run op=view first: it returns each line as `anchor|lineno|text`. \
                    Then pass those anchors to op=replace (span `start`..`end`, or `start` \
                    alone; empty `contents` deletes) or op=insert (`after` or `before`). \
                    An anchor covers a line's position and text, so an edit against a \
                    changed file is rejected instead of applied — re-view and retry."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["view", "replace", "insert"] },
                        "path": { "type": "string" },
                        "start": { "type": "string", "description": "for op=replace: anchor of the first line" },
                        "end": { "type": "string", "description": "for op=replace: anchor of the last line (default: start)" },
                        "after": { "type": "string", "description": "for op=insert: anchor to insert after" },
                        "before": { "type": "string", "description": "for op=insert: anchor to insert before" },
                        "contents": { "type": "string", "description": "replacement or inserted text; required for replace/insert, pass \"\" to delete" }
                    },
                    "required": ["op", "path"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let field = |key: &str| {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            };
            let op = field("op").ok_or(ToolError::InvalidArguments)?;
            let path = field("path").ok_or(ToolError::InvalidArguments)?;
            let content = host_fs::read(&path).map_err(|_| ToolError::ExecutionFailed)?;

            if op == "view" {
                return Ok(guest_fs::truncate(edit::render_view(&content)));
            }

            // `contents` must be present (even empty); absent key would silently delete.
            let contents = field("contents").ok_or(ToolError::InvalidArguments)?;
            let applied = match op.as_str() {
                "replace" => {
                    let start = field("start").ok_or(ToolError::InvalidArguments)?;
                    edit::replace(&content, &start, field("end").as_deref(), &contents)
                }
                "insert" => match (field("after"), field("before")) {
                    (Some(_), Some(_)) => {
                        return Ok(rejected("pass either `after` or `before`, not both."))
                    }
                    (Some(target), None) => edit::insert(&content, &target, false, &contents),
                    (None, Some(target)) => edit::insert(&content, &target, true, &contents),
                    (None, None) => return Err(ToolError::InvalidArguments),
                },
                _ => return Err(ToolError::InvalidArguments),
            };

            match applied {
                Ok((updated, lines)) => {
                    host_fs::write(&path, &updated).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(format!(
                        "{op} applied to {path} ({lines} line(s)). Anchors after the change are \
                         now stale — run op=view before the next edit to this file."
                    ))
                }
                Err(reason) => Ok(rejected(&reason)),
            }
        }
    }

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
