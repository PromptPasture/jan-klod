//! `tool-fs-grep` — find lines matching a substring in a workspace file.
//!
//! `invoke({ "pattern", "path" })` → the matching lines as `lineno:line`, one per
//! line (empty string when nothing matches). Reads through `host-fs` (path-jailed).
//! Component-Model glue only; compiles for `wasm32`.

// The line-matching logic is pure Rust, so it is unit-tested natively; the CM glue
// only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod grep {
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

    #[cfg(test)]
    mod tests {
        use super::matches;

        #[test]
        fn reports_matching_lines_with_numbers() {
            let text = "alpha\nbeta\ngamma beta\ndelta";
            assert_eq!(matches(text, "beta"), "2:beta\n3:gamma beta");
        }

        #[test]
        fn no_match_is_empty() {
            assert_eq!(matches("a\nb", "zzz"), "");
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::grep;

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
                name: "fs-grep".to_string(),
                description: "Find lines matching a substring in a workspace file.".to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["pattern", "path"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let pattern = value.get("pattern").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let path = value.get("path").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let contents = host_fs::read(path).map_err(|_| ToolError::ExecutionFailed)?;
            Ok(grep::matches(&contents, pattern))
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
