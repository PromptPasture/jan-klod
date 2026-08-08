//! `tool-fs` — the single filesystem tool, routed through `host-fs`.
//!
//! Exposes one tool named `fs`; the operation is chosen by an `op` argument:
//!
//! - `{ "op": "read",  "path" }`              → the file's contents.
//! - `{ "op": "write", "path", "contents" }`  → a write confirmation.
//! - `{ "op": "grep",  "pattern", "path"?, "glob"? }` → matching lines.
//!
//! `grep` searches a **directory tree** when `path` names one (the default is the
//! workspace root), and a single file when it names a file. That is the shape the
//! operation is actually used in: "where is this symbol?" is one call, not a
//! `find` followed by a `read` per hit. Tree results are prefixed `path:lineno:line`;
//! single-file results stay `lineno:line`.
//!
//! Output is capped at [`guest_fs::MAX_OUTPUT_BYTES`] so a single result cannot
//! consume the whole context budget, and a tree search is bounded the same way a
//! glob is — with any bound that bites reported in the result. The line-matching
//! logic is pure Rust (unit-tested natively); the Component-Model glue below only
//! compiles for `wasm32`.

/// Most matching lines a tree-wide grep reports. Past this the answer is not a
/// search result any more, it is a haystack — the caller should narrow instead.
const MAX_MATCHES: usize = 400;

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod fs {
    use crate::MAX_MATCHES;

    /// Return `lineno:line` for every 1-based line of `haystack` containing
    /// `pattern`, joined by newlines.
    pub fn matches(haystack: &str, pattern: &str) -> String {
        haystack
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(pattern))
            .map(|(i, line)| format!("{}:{line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A tree-wide grep result: `path:lineno:line` per hit, and whether the match
    /// cap stopped it early.
    pub struct Hits {
        /// One `path:lineno:line` entry per matching line.
        pub lines: Vec<String>,
        /// Set when [`MAX_MATCHES`] cut the search short — a partial result must
        /// never read as "these are all the matches".
        pub capped: bool,
    }

    /// Collect matches across `files`, reading each through `read`.
    ///
    /// `read` is the `host-fs` seam; a file that cannot be read (binary, gone,
    /// not UTF-8) is skipped rather than failing the whole search — one unreadable
    /// file in a tree must not cost the caller the other 200 results.
    pub fn grep_tree(files: &[String], pattern: &str, read: &dyn Fn(&str) -> Option<String>) -> Hits {
        let mut lines = Vec::new();
        for path in files {
            let Some(contents) = read(path) else { continue };
            for (i, line) in contents.lines().enumerate() {
                if line.contains(pattern) {
                    if lines.len() == MAX_MATCHES {
                        return Hits { lines, capped: true };
                    }
                    lines.push(format!("{path}:{}:{line}", i + 1));
                }
            }
        }
        Hits { lines, capped: false }
    }

    /// Format a tree grep, stating every way the result was held back.
    pub fn render(pattern: &str, hits: &Hits, walk_bound: Option<&str>) -> String {
        if hits.lines.is_empty() {
            return format!("no matches for {pattern}");
        }
        let mut parts = vec![hits.lines.join("\n")];
        if hits.capped {
            parts.push(format!("…[partial: stopped at the match cap ({MAX_MATCHES})]"));
        }
        if let Some(bound) = walk_bound {
            parts.push(format!("…[partial: the file walk stopped at the {bound}]"));
        }
        guest_fs::truncate(parts.join("\n"))
    }

    #[cfg(test)]
    mod tests {
        use super::{grep_tree, matches, render, Hits};
        use crate::MAX_MATCHES;

        /// A fake workspace: path → contents. Unknown paths are unreadable.
        fn files<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |path: &str| {
                entries.iter().find(|(p, _)| *p == path).map(|(_, body)| (*body).to_string())
            }
        }

        #[test]
        fn reports_matching_lines_with_numbers() {
            let text = "alpha\nbeta\ngamma beta\ndelta";
            assert_eq!(matches(text, "beta"), "2:beta\n3:gamma beta");
        }

        #[test]
        fn no_match_is_empty() {
            assert_eq!(matches("a\nb", "zzz"), "");
        }

        #[test]
        fn tree_grep_prefixes_each_hit_with_its_path() {
            let read = files(&[("a.rs", "fn one() {}\nfn two() {}"), ("b.rs", "fn three() {}")]);
            let hits = grep_tree(
                &["a.rs".to_string(), "b.rs".to_string()],
                "fn t",
                &read,
            );
            assert_eq!(hits.lines, vec!["a.rs:2:fn two() {}", "b.rs:1:fn three() {}"]);
            assert!(!hits.capped);
        }

        #[test]
        fn an_unreadable_file_is_skipped_not_fatal() {
            let read = files(&[("b.rs", "hit")]);
            // "a.rs" is absent from the fake workspace (binary/gone/not UTF-8).
            let hits = grep_tree(&["a.rs".to_string(), "b.rs".to_string()], "hit", &read);
            assert_eq!(hits.lines, vec!["b.rs:1:hit"]);
        }

        #[test]
        fn the_match_cap_bounds_a_tree_grep_and_is_reported() {
            let body = "hit\n".repeat(MAX_MATCHES + 50);
            let entries = [("big.rs", body.as_str())];
            let read = files(&entries);
            let hits = grep_tree(&["big.rs".to_string()], "hit", &read);
            assert_eq!(hits.lines.len(), MAX_MATCHES);
            assert!(hits.capped);
            assert!(render("hit", &hits, None).contains("stopped at the match cap"));
        }

        #[test]
        fn no_match_renders_as_a_statement() {
            let hits = Hits { lines: vec![], capped: false };
            assert_eq!(render("zzz", &hits, None), "no matches for zzz");
        }

        #[test]
        fn a_bounded_walk_is_reported_alongside_the_hits() {
            let hits = Hits { lines: vec!["a.rs:1:x".to_string()], capped: false };
            let rendered = render("x", &hits, Some("visit budget"));
            assert!(rendered.starts_with("a.rs:1:x\n"), "hits come first: {rendered}");
            assert!(rendered.contains("file walk stopped at the visit budget"), "{rendered}");
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::fs;

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
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

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "fs".to_string(),
                description: "Read, write, or grep workspace files. Choose the operation \
                    with `op` (read | write | grep). `grep` searches a whole directory \
                    tree when `path` is a directory (the default is the workspace root), \
                    optionally narrowed to files matching `glob`."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["read", "write", "grep"] },
                        "path": {
                            "type": "string",
                            "description": "file for read/write; file or directory for grep \
                                (default: workspace root)"
                        },
                        "contents": { "type": "string", "description": "for op=write" },
                        "pattern": { "type": "string", "description": "for op=grep" },
                        "glob": {
                            "type": "string",
                            "description": "for op=grep over a directory: only search files \
                                matching this glob, e.g. `**/*.rs` (default: all files)"
                        }
                    },
                    "required": ["op"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let op = value
                .get("op")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;
            let path = value.get("path").and_then(serde_json::Value::as_str);

            match op {
                "read" => {
                    let path = path.ok_or(ToolError::InvalidArguments)?;
                    let content = host_fs::read(path).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(guest_fs::truncate(content))
                }
                "grep" => {
                    let pattern = value
                        .get("pattern")
                        .and_then(serde_json::Value::as_str)
                        .ok_or(ToolError::InvalidArguments)?;
                    let glob = value.get("glob").and_then(serde_json::Value::as_str).unwrap_or("**/*");
                    grep(path.unwrap_or("."), pattern, glob)
                }
                "write" => {
                    let path = path.ok_or(ToolError::InvalidArguments)?;
                    let contents =
                        value.get("contents").and_then(serde_json::Value::as_str).unwrap_or("");
                    host_fs::write(path, contents).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(format!("wrote {path} ({} bytes)", contents.len()))
                }
                _ => Err(ToolError::InvalidArguments),
            }
        }
    }

    /// Grep a file or a whole tree, deciding which by probing `list-dir`: a path
    /// that lists is a directory, anything else is read as a file. (`host-fs` has
    /// no `is-dir`, and adding one to the contract for a two-branch dispatch is not
    /// worth widening the interface.)
    fn grep(path: &str, pattern: &str, glob: &str) -> Result<String, ToolError> {
        if host_fs::list_dir(path).is_err() {
            let content = host_fs::read(path).map_err(|_| ToolError::ExecutionFailed)?;
            return Ok(guest_fs::truncate(fs::matches(&content, pattern)));
        }
        let found = guest_fs::walk(path, glob, &|dir: &str| {
            host_fs::list_dir(dir).ok().map(|entries| {
                entries
                    .into_iter()
                    .map(|e| guest_fs::Entry { name: e.name, is_dir: e.is_dir })
                    .collect()
            })
        });
        let hits = fs::grep_tree(&found.paths, pattern, &|p: &str| host_fs::read(p).ok());
        Ok(fs::render(pattern, &hits, found.bounded_by))
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
