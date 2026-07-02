//! `tool-fs-probe` — a `tool-callable` guest exercising the `host-fs` capability.
//!
//! Its `invoke({ "path", "contents" })` writes `contents` to the workspace-relative
//! `path` then reads it back, returning the read-back text — proving `host-fs`
//! works across the Component-Model boundary (and that path-jail denials surface as
//! errors). Entirely Component-Model glue, so it only compiles for `wasm32`.

#[cfg(target_arch = "wasm32")]
mod component {
    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod bindings {
        wit_bindgen::generate!({
            world: "tool-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_fs;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-fs-probe", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
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
                name: "fs-probe".to_string(),
                description: "Write then read back a workspace file (host-fs probe).".to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "contents": { "type": "string" }
                    },
                    "required": ["path"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let path = value.get("path").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let contents = value.get("contents").and_then(serde_json::Value::as_str).unwrap_or("");

            host_fs::write(path, contents).map_err(|err| {
                log(LogLevel::Warn, &format!("write {path} failed: {err:?}"));
                ToolError::ExecutionFailed
            })?;
            host_fs::read(path).map_err(|err| {
                log(LogLevel::Warn, &format!("read {path} failed: {err:?}"));
                ToolError::ExecutionFailed
            })
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
