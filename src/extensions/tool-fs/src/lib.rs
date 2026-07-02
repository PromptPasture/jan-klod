//! `tool-fs` — the single filesystem tool, routed through `host-fs`.
//!
//! Exposes one tool named `fs`; the operation is chosen by an `op` argument:
//!
//! - `{ "op": "read",  "path" }`              → the file's contents.
//! - `{ "op": "write", "path", "contents" }`  → a write confirmation.
//! - `{ "op": "grep",  "path", "pattern" }`   → matching lines as `lineno:line`.
//!
//! `read`/`grep` output is capped at [`MAX_OUTPUT_BYTES`] so a single result cannot
//! consume the whole context budget. The line-matching and truncation logic is pure
//! Rust (unit-tested natively); the Component-Model glue below only compiles for
//! `wasm32`.

/// Tool-result byte cap — keeps a single file (or grep result) from consuming the
/// entire context budget.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod fs {
    use crate::MAX_OUTPUT_BYTES;

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

    /// Cap `output` at [`MAX_OUTPUT_BYTES`], backing off to a valid UTF-8 boundary
    /// and appending a truncation marker when it overflows.
    pub fn truncate(output: String) -> String {
        if output.len() <= MAX_OUTPUT_BYTES {
            return output;
        }
        // Back off to a valid UTF-8 boundary at or below the cap; slicing at a
        // non-boundary would panic.
        let mut safe_len = MAX_OUTPUT_BYTES;
        while safe_len > 0 && !output.is_char_boundary(safe_len) {
            safe_len -= 1;
        }
        format!("{}\n…[truncated: {} bytes omitted]", &output[..safe_len], output.len() - safe_len)
    }

    #[cfg(test)]
    mod tests {
        use super::{matches, truncate, MAX_OUTPUT_BYTES};

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
        fn output_under_cap_passes_through() {
            let small = "1:hello\n2:world".to_string();
            assert_eq!(truncate(small.clone()), small);
        }

        #[test]
        fn output_over_cap_is_truncated_with_marker() {
            let big = "x".repeat(MAX_OUTPUT_BYTES + 100);
            let result = truncate(big);
            assert!(result.len() < MAX_OUTPUT_BYTES + 200);
            assert!(result.contains("…[truncated:"), "truncation marker must be present");
        }

        #[test]
        fn truncation_at_multibyte_boundary_does_not_panic() {
            // A 2-byte char ('é') repeated so the cap lands mid-character.
            let big = "é".repeat(MAX_OUTPUT_BYTES);
            let result = truncate(big);
            assert!(result.contains("…[truncated:"), "truncation marker must be present");
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
                description: "Read, write, or grep a UTF-8 file in the workspace. \
                    Choose the operation with `op` (read | write | grep)."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["read", "write", "grep"] },
                        "path": { "type": "string" },
                        "contents": { "type": "string", "description": "for op=write" },
                        "pattern": { "type": "string", "description": "for op=grep" }
                    },
                    "required": ["op", "path"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let op = value.get("op").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let path = value.get("path").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;

            match op {
                "read" => {
                    let content = host_fs::read(path).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(fs::truncate(content))
                }
                "grep" => {
                    let pattern = value
                        .get("pattern")
                        .and_then(serde_json::Value::as_str)
                        .ok_or(ToolError::InvalidArguments)?;
                    let content = host_fs::read(path).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(fs::truncate(fs::matches(&content, pattern)))
                }
                "write" => {
                    let contents = value.get("contents").and_then(serde_json::Value::as_str).unwrap_or("");
                    host_fs::write(path, contents).map_err(|_| ToolError::ExecutionFailed)?;
                    Ok(format!("wrote {path} ({} bytes)", contents.len()))
                }
                _ => Err(ToolError::InvalidArguments),
            }
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
