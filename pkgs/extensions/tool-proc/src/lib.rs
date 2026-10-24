//! `tool-proc` — start, read and stop long-lived children, by granted name only.
//!
//! Commands come only from `execution.long-lived`, and handles stay in this instance. Refusals are `Ok` results because `tool-error` carries no message.

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

    use std::cell::RefCell;

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::host_process;

    /// Most bytes one `output` call returns; the host's output cap bounds it further.
    const READ_BYTES: u32 = 8192;
    /// How long `output` waits for the first byte.
    ///
    /// Short on purpose: an empty read means "nothing yet", and blocking a turn is worse.
    /// `running` answers whether the child has finished.
    const READ_WAIT_MS: u32 = 200;

    thread_local! {
        /// `name -> handle` for children this instance started.
        /// A `Vec` is enough: at most one entry per granted name, and it keeps start order.
        static STARTED: RefCell<Vec<(String, u32)>> = const { RefCell::new(Vec::new()) };
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-proc", message, &[]);
    }

    /// The handle for `name`, if this instance started it.
    fn handle(name: &str) -> Option<u32> {
        STARTED.with_borrow(|started| {
            started
                .iter()
                .find(|(started_name, _)| started_name == name)
                .map(|(_, handle)| *handle)
        })
    }

    /// A refusal the model can act on.
    fn refused(reason: &str) -> String {
        serde_json::json!({ "refused": reason }).to_string()
    }

    /// A refusal for an ungranted name. Lists the granted names, since a model that cannot see them cannot correct itself.
    fn no_such_child(name: &str) -> String {
        serde_json::json!({
            "refused": format!("no long-lived child named `{name}` is granted"),
            "granted": host_process::granted(),
        })
        .to_string()
    }

    /// Every granted name, and whether this session has it running.
    fn list() -> String {
        let children: Vec<serde_json::Value> = host_process::granted()
            .into_iter()
            .map(|name| {
                let running = handle(&name).is_some_and(host_process::is_running);
                serde_json::json!({ "name": name, "running": running })
            })
            .collect();
        serde_json::json!({ "children": children }).to_string()
    }

    /// Start a granted child, or report the one already running.
    fn start(name: &str) -> String {
        if handle(name).is_some_and(host_process::is_running) {
            return serde_json::json!({ "name": name, "started": false, "running": true })
                .to_string();
        }
        match host_process::spawn(name) {
            Ok(child) => {
                STARTED.with_borrow_mut(|started| {
                    started.retain(|(started_name, _)| started_name != name);
                    started.push((name.to_string(), child));
                });
                log(LogLevel::Info, &format!("started `{name}`"));
                serde_json::json!({ "name": name, "started": true, "running": true }).to_string()
            }
            Err(host_process::ProcError::Denied) => no_such_child(name),
            Err(err) => {
                log(LogLevel::Warn, &format!("`{name}` did not start ({err:?})"));
                refused(&format!("`{name}` could not be started"))
            }
        }
    }

    /// Whatever the child has printed since the last call.
    fn output(name: &str) -> String {
        let Some(child) = handle(name) else {
            return not_started(name);
        };
        let out = host_process::read_stdout(child, READ_BYTES, READ_WAIT_MS).unwrap_or_default();
        serde_json::json!({
            "name": name,
            "output": out,
            "running": host_process::is_running(child),
        })
        .to_string()
    }

    /// Stop a child this session started.
    fn stop(name: &str) -> String {
        let Some(child) = handle(name) else {
            return not_started(name);
        };
        host_process::kill(child);
        STARTED.with_borrow_mut(|started| {
            started.retain(|(started_name, _)| started_name != name);
        });
        log(LogLevel::Info, &format!("stopped `{name}`"));
        serde_json::json!({ "name": name, "stopped": true }).to_string()
    }

    /// A granted name this session has not started, which needs a different next move from an ungranted one.
    fn not_started(name: &str) -> String {
        if host_process::granted()
            .iter()
            .any(|granted| granted == name)
        {
            return refused(&format!(
                "`{name}` is granted but not running; start it first"
            ));
        }
        no_such_child(name)
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
            log(LogLevel::Info, "started; long-lived children by name");
            Ok(())
        }
        /// The host kills any children still running when this instance stops.
        fn stop() {
            log(LogLevel::Info, "stopping");
        }
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "proc".to_string(),
                description: "Start, read and stop a long-lived background process by name. \
                              The available names are fixed by this deployment — use `list` \
                              to see them. You cannot choose what a name runs."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": {
                            "enum": ["list", "start", "output", "stop"],
                            "type": "string"
                        },
                        "name": {
                            "type": "string",
                            "description": "which granted child; required for all but list"
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
            if op == "list" {
                return Ok(list());
            }
            let Some(name) = value.get("name").and_then(serde_json::Value::as_str) else {
                return Err(ToolError::InvalidArguments);
            };
            Ok(match op {
                "start" => start(name),
                "output" => output(name),
                "stop" => stop(name),
                _ => refused(&format!("unknown op `{op}`")),
            })
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
