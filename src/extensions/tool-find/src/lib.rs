//! `tool-find` — glob search over the workspace tree, routed through `host-fs`.
//!
//! Exposes one tool named `find`:
//!
//! - `{ "pattern": "**/*.rs" }`               → every Rust file in the workspace.
//! - `{ "pattern": "*.toml", "path": "src" }` → matches directly under `src/`.
//!
//! The tool is the discovery half of the file fleet: `tool-fs` can `read`/`grep` a
//! path it was *given*, but nothing could enumerate paths. The walk is guest-side
//! over [`host-fs`]'s `list-dir`, so it sees exactly what the jail exposes and
//! nothing else — there is no host-side directory-walking capability to grant.
//!
//! The matcher, the bounded walk, and the output cap live in the shared
//! [`guest_fs`] library (unit-tested natively there); this crate holds only the
//! rendering and the Component-Model glue, which compiles for `wasm32` alone.

/// Format a walk as tool output: matching paths, or an explicit no-match line,
/// plus a note whenever a bound (or the byte cap) held results back.
///
/// A truncated tree must never read as an exhaustive one, so every way the result
/// was held back is stated in the result itself.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn render(pattern: &str, outcome: &guest_fs::Outcome) -> String {
    if outcome.paths.is_empty() {
        return format!("no files match {pattern}");
    }
    let mut kept = outcome.paths.as_slice();
    let mut used = 0usize;
    if let Some(over) = outcome.paths.iter().position(|path| {
        used += path.len() + 1;
        used > guest_fs::MAX_OUTPUT_BYTES
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

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::render;

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

            let outcome = guest_fs::walk(root, pattern, &list_dir);
            Ok(render(pattern, &outcome))
        }
    }

    /// The `host-fs` `list-dir` seam the shared walk drives. `None` (unreadable)
    /// means "skip this subtree", never "fail the call".
    fn list_dir(dir: &str) -> Option<Vec<guest_fs::Entry>> {
        host_fs::list_dir(dir).ok().map(|entries| {
            entries
                .into_iter()
                .map(|e| guest_fs::Entry { name: e.name, is_dir: e.is_dir })
                .collect()
        })
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use guest_fs::{Outcome, MAX_OUTPUT_BYTES};

    #[test]
    fn no_match_renders_as_a_statement_not_an_empty_string() {
        let outcome = Outcome { paths: vec![], bounded_by: None };
        assert_eq!(render("**/*.zig", &outcome), "no files match **/*.zig");
    }

    #[test]
    fn a_bounded_walk_says_it_is_partial() {
        let outcome = Outcome { paths: vec!["a.rs".to_string()], bounded_by: Some("result cap") };
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
}
