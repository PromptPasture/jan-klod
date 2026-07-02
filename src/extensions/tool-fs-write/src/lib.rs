//! `tool-fs-write` — write a workspace file through `host-fs`.
//!
//! `invoke({ "path", "contents" })` → a confirmation. Component-Model glue only;
//! compiles for `wasm32`.

#[cfg(target_arch = "wasm32")]
mod component {
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
                name: "fs-write".to_string(),
                description: "Create or replace a UTF-8 file in the workspace.".to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "contents": { "type": "string" }
                    },
                    "required": ["path", "contents"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let path = value.get("path").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let contents = value.get("contents").and_then(serde_json::Value::as_str).unwrap_or("");
            host_fs::write(path, contents).map_err(|_| ToolError::ExecutionFailed)?;
            Ok(format!("wrote {path} ({} bytes)", contents.len()))
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
