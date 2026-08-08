//! `tool-find` — glob search over the workspace tree, routed through `host-fs`.
//!
//! Exposes one tool named `find`:
//!
//! - `{ "pattern": "**/*.rs" }`            → every Rust file in the workspace.
//! - `{ "pattern": "*.toml", "path": "src" }` → matches directly under `src/`.
//!
//! The tool is the discovery half of the file fleet: `tool-fs` can `read`/`grep` a
//! path it was *given*, but nothing could enumerate paths. The walk is guest-side
//! over [`host-fs`]'s `list-dir`, so it sees exactly what the jail exposes and
//! nothing else — there is no host-side directory-walking capability to grant.
//!
//! Every dimension of the walk is bounded ([`MAX_VISITS`], [`MAX_DEPTH`],
//! [`MAX_RESULTS`], [`MAX_OUTPUT_BYTES`]), because a glob is the one file operation
//! whose cost is set by the *tree*, not by the argument: `**/*` in a monorepo must
//! not be able to hang the turn or eat the context budget. When a bound bites, the
//! result says so rather than silently reporting a partial tree as complete.
//!
//! The matcher and the walk are pure Rust (unit-tested natively); the
//! Component-Model glue below only compiles for `wasm32`.

/// Result byte cap — one glob must not consume the whole context budget.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Most paths reported from a single call.
const MAX_RESULTS: usize = 500;
/// Most directory entries examined during one walk.
const MAX_VISITS: usize = 20_000;
/// Deepest directory nesting descended into (relative to the search root).
const MAX_DEPTH: usize = 24;

/// Directories skipped unless the pattern names them literally. These are the
/// build/vendor trees that dominate a walk's cost while almost never being what
/// the caller meant; naming one in the pattern (`target/**/*.rs`) opts back in.
const PRUNED_DIRS: [&str; 6] =
    [".git", "node_modules", "target", ".venv", "__pycache__", ".jj"];

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod find {
    use crate::{MAX_DEPTH, MAX_OUTPUT_BYTES, MAX_RESULTS, MAX_VISITS, PRUNED_DIRS};

    /// One directory entry, as `host-fs.entry` hands it over.
    pub struct Entry {
        /// Bare file or directory name (not a path).
        pub name: String,
        /// Whether the entry is a directory.
        pub is_dir: bool,
    }

    /// What a walk found, and which bound (if any) cut it short.
    pub struct Outcome {
        /// Matching paths, workspace-relative and sorted.
        pub paths: Vec<String>,
        /// Set when a bound stopped the walk before the tree was exhausted, so a
        /// caller never reads a partial answer as an exhaustive one.
        pub bounded_by: Option<&'static str>,
    }

    /// Expand a slash-less pattern to search recursively.
    ///
    /// Strict glob semantics make `*.rs` mean "top level only", which is almost
    /// never what a caller (or a small model) means when it asks to *find* a file.
    /// A pattern that spells out any structure is left exactly as written.
    pub fn normalize(pattern: &str) -> String {
        if pattern.contains('/') {
            pattern.to_string()
        } else {
            format!("**/{pattern}")
        }
    }

    /// Whether `path` matches glob `pattern`.
    ///
    /// `*` and `?` match within one path segment; `**` matches any run of segments,
    /// including none.
    pub fn matches(pattern: &str, path: &str) -> bool {
        let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
        let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        match_segments(&pat, &seg)
    }

    fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
        match pat.split_first() {
            None => seg.is_empty(),
            Some((&"**", rest)) => (0..=seg.len()).any(|i| match_segments(rest, &seg[i..])),
            Some((head, rest)) => match seg.split_first() {
                Some((name, tail)) => match_name(head, name) && match_segments(rest, tail),
                None => false,
            },
        }
    }

    /// Match one path segment against a `*`/`?` pattern (neither crosses `/`).
    fn match_name(pattern: &str, name: &str) -> bool {
        let pat: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = name.chars().collect();
        let (mut pi, mut ti) = (0, 0);
        // Backtrack point: the last `*` seen, and how much of `text` it had eaten.
        let mut star: Option<usize> = None;
        let mut eaten = 0;
        while ti < text.len() {
            if pi < pat.len() && (pat[pi] == '?' || pat[pi] == text[ti]) {
                pi += 1;
                ti += 1;
            } else if pi < pat.len() && pat[pi] == '*' {
                star = Some(pi);
                eaten = ti;
                pi += 1;
            } else if let Some(s) = star {
                // Let the `*` swallow one more char and retry from just after it.
                eaten += 1;
                ti = eaten;
                pi = s + 1;
            } else {
                return false;
            }
        }
        pat[pi..].iter().all(|c| *c == '*')
    }

    /// Whether the walk should descend into a directory named `name`.
    ///
    /// Heavy build/vendor trees are pruned unless the pattern mentions them, so
    /// opting back in is a matter of asking for them by name.
    fn should_descend(name: &str, pattern: &str) -> bool {
        !PRUNED_DIRS.contains(&name)
            || pattern.split('/').any(|segment| segment == name)
    }

    /// Walk `root` breadth-first through `list`, collecting files matching `pattern`.
    ///
    /// `list` is the `host-fs` `list-dir` seam (returning `None` when a directory
    /// cannot be read — an unreadable subtree is skipped, never fatal). Paths are
    /// matched *relative to `root`* and reported joined back onto it.
    pub fn walk(root: &str, pattern: &str, list: &dyn Fn(&str) -> Option<Vec<Entry>>) -> Outcome {
        let pattern = normalize(pattern);
        let mut paths = Vec::new();
        let mut bounded_by = None;
        let mut visits = 0usize;
        // (path relative to root, depth); "" is the root itself.
        let mut queue = vec![(String::new(), 0usize)];

        while let Some((dir, depth)) = queue.pop() {
            let Some(entries) = list(&join(root, &dir)) else { continue };
            for entry in entries {
                visits += 1;
                if visits > MAX_VISITS {
                    bounded_by = Some("visit budget");
                    queue.clear();
                    break;
                }
                let rel = join(&dir, &entry.name);
                if entry.is_dir {
                    if depth + 1 < MAX_DEPTH && should_descend(&entry.name, &pattern) {
                        queue.push((rel, depth + 1));
                    }
                } else if matches(&pattern, &rel) {
                    if paths.len() == MAX_RESULTS {
                        bounded_by = Some("result cap");
                        queue.clear();
                        break;
                    }
                    paths.push(join(root, &rel));
                }
            }
        }

        paths.sort();
        Outcome { paths, bounded_by }
    }

    /// Format a walk as tool output: matching paths, or an explicit no-match line,
    /// plus a note whenever a bound (or the byte cap) held results back.
    pub fn render(pattern: &str, outcome: &Outcome) -> String {
        if outcome.paths.is_empty() {
            return format!("no files match {pattern}");
        }
        let mut kept = outcome.paths.as_slice();
        let mut used = 0usize;
        if let Some(over) = outcome.paths.iter().position(|path| {
            used += path.len() + 1;
            used > MAX_OUTPUT_BYTES
        }) {
            kept = &outcome.paths[..over];
        }
        let omitted = outcome.paths.len() - kept.len();
        let mut parts = vec![kept.join("\n")];
        if let Some(bound) = outcome.bounded_by {
            parts.push(format!("…[partial: stopped at the {bound}]"));
        }
        if omitted > 0 {
            parts.push(format!("…[truncated: {omitted} more paths omitted]"));
        }
        parts.join("\n")
    }

    /// Join two workspace-relative fragments, treating `""` and `"."` as "here".
    fn join(base: &str, rest: &str) -> String {
        let base = if base == "." { "" } else { base };
        match (base.is_empty(), rest.is_empty()) {
            (true, _) => rest.to_string(),
            (false, true) => base.to_string(),
            (false, false) => format!("{base}/{rest}"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{join, matches, normalize, render, walk, Entry, Outcome};
        use crate::{MAX_OUTPUT_BYTES, MAX_RESULTS, PRUNED_DIRS};

        /// A fake tree: directory path → entries. Root is `""`.
        fn tree<'a>(
            dirs: &'a [(&'a str, &'a [(&'a str, bool)])],
        ) -> impl Fn(&str) -> Option<Vec<Entry>> + 'a {
            move |path: &str| {
                let key = if path == "." { "" } else { path };
                dirs.iter().find(|(d, _)| *d == key).map(|(_, entries)| {
                    entries
                        .iter()
                        .map(|(name, is_dir)| Entry { name: (*name).to_string(), is_dir: *is_dir })
                        .collect()
                })
            }
        }

        #[test]
        fn star_stays_within_one_segment() {
            assert!(matches("*.rs", "lib.rs"));
            assert!(!matches("*.rs", "src/lib.rs"));
            assert!(matches("src/*.rs", "src/lib.rs"));
        }

        #[test]
        fn double_star_spans_segments_including_none() {
            assert!(matches("**/*.rs", "lib.rs"));
            assert!(matches("**/*.rs", "a/b/c/lib.rs"));
            assert!(matches("src/**/mod.rs", "src/mod.rs"));
            assert!(matches("src/**/mod.rs", "src/a/b/mod.rs"));
            assert!(!matches("src/**/mod.rs", "other/a/mod.rs"));
        }

        #[test]
        fn question_mark_matches_exactly_one_char() {
            assert!(matches("v?.txt", "v1.txt"));
            assert!(!matches("v?.txt", "v10.txt"));
        }

        #[test]
        fn backtracking_star_matches_repeated_prefixes() {
            // Naive greedy matching fails this; the `*` must give chars back.
            assert!(matches("*abc", "aababc"));
            assert!(matches("a*b*c", "axxbyyc"));
            assert!(!matches("a*b*c", "axxbyy"));
        }

        #[test]
        fn slashless_patterns_become_recursive() {
            assert_eq!(normalize("*.rs"), "**/*.rs");
            assert_eq!(normalize("src/*.rs"), "src/*.rs");
        }

        #[test]
        fn walk_finds_matches_across_the_tree_sorted() {
            let list = tree(&[
                ("", &[("src", true), ("README.md", false)]),
                ("src", &[("main.rs", false), ("util", true)]),
                ("src/util", &[("helper.rs", false), ("notes.md", false)]),
            ]);
            let found = walk(".", "*.rs", &list);
            assert_eq!(found.paths, vec!["src/main.rs", "src/util/helper.rs"]);
            assert!(found.bounded_by.is_none());
        }

        #[test]
        fn walk_reports_paths_relative_to_the_workspace_not_the_root() {
            let list = tree(&[
                ("src", &[("main.rs", false), ("util", true)]),
                ("src/util", &[("helper.rs", false)]),
            ]);
            // Pattern is matched relative to `src`, results are prefixed with it.
            let found = walk("src", "*.rs", &list);
            assert_eq!(found.paths, vec!["src/main.rs", "src/util/helper.rs"]);
        }

        #[test]
        fn heavy_dirs_are_pruned_unless_named() {
            let dirs: Vec<(&str, &[(&str, bool)])> = vec![
                ("", &[("target", true), ("src", true)]),
                ("target", &[("build.rs", false)]),
                ("src", &[("main.rs", false)]),
            ];
            let list = tree(&dirs);
            assert!(PRUNED_DIRS.contains(&"target"));
            assert_eq!(walk(".", "**/*.rs", &list).paths, vec!["src/main.rs"]);
            // Naming the pruned directory opts back in.
            assert_eq!(walk(".", "target/**/*.rs", &list).paths, vec!["target/build.rs"]);
        }

        #[test]
        fn an_unreadable_directory_is_skipped_not_fatal() {
            let list = tree(&[("", &[("ghost", true), ("a.rs", false)])]);
            // "ghost" has no entry in the fake tree → listing it returns None.
            assert_eq!(walk(".", "*.rs", &list).paths, vec!["a.rs"]);
        }

        #[test]
        fn the_result_cap_bounds_the_walk_and_is_reported() {
            let many: Vec<(String, bool)> =
                (0..MAX_RESULTS + 50).map(|i| (format!("f{i}.rs"), false)).collect();
            let list = |path: &str| {
                (path == "." || path.is_empty()).then(|| {
                    many.iter()
                        .map(|(name, is_dir)| Entry { name: name.clone(), is_dir: *is_dir })
                        .collect()
                })
            };
            let found = walk(".", "*.rs", &list);
            assert_eq!(found.paths.len(), MAX_RESULTS);
            assert_eq!(found.bounded_by, Some("result cap"));
        }

        #[test]
        fn no_match_renders_as_a_statement_not_an_empty_string() {
            let outcome = Outcome { paths: vec![], bounded_by: None };
            assert_eq!(render("**/*.zig", &outcome), "no files match **/*.zig");
        }

        #[test]
        fn a_bounded_walk_says_it_is_partial() {
            let outcome =
                Outcome { paths: vec!["a.rs".to_string()], bounded_by: Some("result cap") };
            let rendered = render("*.rs", &outcome);
            assert!(rendered.starts_with("a.rs\n"), "paths come first: {rendered}");
            assert!(rendered.contains("partial: stopped at the result cap"), "{rendered}");
        }

        #[test]
        fn output_over_the_byte_cap_is_truncated_with_a_count() {
            let long = "d/".repeat(60) + "f.rs"; // ~124 bytes per path
            let paths: Vec<String> = (0..600).map(|i| format!("{i}{long}")).collect();
            let total = paths.len();
            let rendered = render("**/*.rs", &Outcome { paths, bounded_by: None });
            assert!(rendered.len() <= MAX_OUTPUT_BYTES + 64, "capped: {} bytes", rendered.len());
            let kept = rendered.lines().filter(|l| !l.starts_with('…')).count();
            assert!(kept > 0 && kept < total, "some but not all paths kept: {kept}/{total}");
            assert!(
                rendered.contains(&format!("{} more paths omitted", total - kept)),
                "omitted count must match what was dropped: {rendered}"
            );
        }

        #[test]
        fn joining_treats_dot_and_empty_as_here() {
            assert_eq!(join(".", "src"), "src");
            assert_eq!(join("", "src"), "src");
            assert_eq!(join("src", ""), "src");
            assert_eq!(join("src", "lib.rs"), "src/lib.rs");
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::find;

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
                name: "find".to_string(),
                description: "Find workspace files by glob pattern. `*` and `?` match \
                    within one path segment, `**` spans directories; a pattern with no \
                    `/` searches recursively. Returns matching paths, one per line."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "glob, e.g. `**/*.rs` or `Cargo.toml`"
                        },
                        "path": {
                            "type": "string",
                            "description": "directory to search from (default: workspace root)"
                        }
                    },
                    "required": ["pattern"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let pattern = value
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;
            let root = value.get("path").and_then(serde_json::Value::as_str).unwrap_or(".");

            // A root the jail refuses is the tool's only hard failure: every other
            // unreadable directory is simply not walked.
            if host_fs::list_dir(root).is_err() {
                return Err(ToolError::ExecutionFailed);
            }

            let outcome = find::walk(root, pattern, &|dir: &str| {
                host_fs::list_dir(dir).ok().map(|entries| {
                    entries
                        .into_iter()
                        .map(|e| find::Entry { name: e.name, is_dir: e.is_dir })
                        .collect()
                })
            });

            Ok(find::render(pattern, &outcome))
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
