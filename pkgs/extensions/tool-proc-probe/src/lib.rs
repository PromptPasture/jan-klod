//! `tool-proc-probe` — exercises the `host-process` capability.
//!
//! Exercises `host-process`: `invoke` with `command` returns the command's stdout.

#[cfg(target_arch = "wasm32")]
mod component {
    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
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
            log(
                LogLevel::Info,
                &format!("init id={} version={}", ctx.id, ctx.version),
            );
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
                description: "Run a command via host-process and return its stdout, \
                              with {\"spawn\": name} ask for a long-lived child, \
                              or with {\"granted\": true} list the names granted."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "args": { "type": "array", "items": { "type": "string" } },
                        "granted": { "type": "boolean" },
                        "spawn": { "type": "string" },
                        "send": { "type": "string" },
                        "leak": { "type": "boolean" }
                    }
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;

            // `granted: true` lists the long-lived names as text, so tests need no result format.
            if value
                .get("granted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(format!("granted {}", host_process::granted().join(" ")));
            }

            // Outcome is text, not error: ToolError carries no message, so a refusal would look like a crash.
            if let Some(name) = value.get("spawn").and_then(serde_json::Value::as_str) {
                let child = match host_process::spawn(name) {
                    Ok(child) => child,
                    Err(err) => {
                        log(LogLevel::Warn, &format!("spawn {name} refused ({err:?})"));
                        return Ok(format!("spawn-refused {name} {err:?}"));
                    }
                };
                // Write then read back: a handle alone proves only that a number was issued.
                if let Some(send) = value.get("send").and_then(serde_json::Value::as_str) {
                    if let Err(err) = host_process::write_stdin(child, send) {
                        log(LogLevel::Warn, &format!("write-stdin failed ({err:?})"));
                    }
                }
                let out = host_process::read_stdout(child, 4096, 2000).unwrap_or_default();
                let running = host_process::is_running(child);
                // `leak: true` skips the kill, to exercise the host's lifetime guarantee.
                if !value
                    .get("leak")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    host_process::kill(child);
                }
                return Ok(format!("spawned {name} running={running} out={out}"));
            }

            let command = value
                .get("command")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;
            let args: Vec<String> = value
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            let exit = host_process::exec(command, &args, None, None).map_err(|err| {
                log(
                    LogLevel::Warn,
                    &format!("exec {command} was refused or failed ({err:?})"),
                );
                ToolError::ExecutionFailed
            })?;
            Ok(exit.stdout)
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
