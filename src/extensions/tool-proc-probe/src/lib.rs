//! `tool-proc-probe` — a `tool-callable` guest exercising the `host-process`
//! capability.
//!
//! Its `invoke({ "command", "args"? })` runs the command through `host-process`
//! and returns its stdout — proving `host-process` works across the Component-Model
//! boundary (and that default-deny / bounds surface as errors). Component-Model glue
//! only, so it compiles for `wasm32` only.

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
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::host_process;

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-proc-probe", message, &[]);
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
                name: "proc-probe".to_string(),
                description: "Run a command via host-process and return its stdout.".to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "args": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["command"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let command = value.get("command").and_then(serde_json::Value::as_str).ok_or(ToolError::InvalidArguments)?;
            let args: Vec<String> = value
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();

            let exit = host_process::exec(command, &args, None, None).map_err(|err| {
                log(LogLevel::Warn, &format!("exec {command} failed: {err:?}"));
                ToolError::ExecutionFailed
            })?;
            Ok(exit.stdout)
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
