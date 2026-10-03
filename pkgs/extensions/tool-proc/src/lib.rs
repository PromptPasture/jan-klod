//! `tool-proc` — the model's side of a long-lived child (#220).
//!
//! The host has held children open since #109, but only a *guest* could
//! drive one. A model could run a command to completion and nothing else:
//! no starting a dev server, no reading what it has printed since, no
//! stopping it. This is that half.
//!
//! # By name, never by command
//!
//! Every operation names a child the operator wrote down in
//! `execution.long-lived`; the command and its arguments come from config
//! and are never in this component's hands. That is deliberate and is the
//! reason `execution.long-lived` is narrower than `execution.enabled`: a
//! tool that let the model choose the command would collapse the two, and
//! the grant would stop meaning anything.
//!
//! So the widest thing a model can do here is start something already
//! written down, read it, and stop it.
//!
//! # Handles never reach the model
//!
//! `host-process` issues a `u32` per child. The model gets names. The map
//! between them lives in this instance's memory, which is the right home:
//! a handle is meaningless outside the instance that was issued it, so
//! `host-storage` — which survives instances — would be storing a number
//! that is already stale by the time it is read back. A tool instance
//! lives as long as its session, so a child started in one turn is still
//! addressable in the next, which is the whole point.
//!
//! # A refusal is a result, not an error
//!
//! `tool-error` carries no message. A refusal raised as one is
//! indistinguishable from a crash, and "you asked for a child nobody
//! granted" has to read differently from "the tool broke". So refusals
//! come back as `Ok` with `"refused"` and a reason — and, when the reason
//! is an unknown name, with the names that *are* available, since a model
//! that cannot see them cannot correct itself.
//!
//! Component-Model glue only, so it compiles for `wasm32` only.

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

    /// Most bytes one `output` call returns. The host's own output cap
    /// bounds it further; this only stops a single read from being the
    /// whole of a chatty server's hour.
    const READ_BYTES: u32 = 8192;
    /// How long `output` waits for the first byte.
    ///
    /// Short on purpose: an empty answer means "nothing yet", which the
    /// model can act on, and a tool call that blocks the turn for seconds
    /// to say the same thing is worse. `host-process` documents the empty
    /// string as "nothing arrived", distinct from "finished" — `running`
    /// below is what answers the second question.
    const READ_WAIT_MS: u32 = 200;

    thread_local! {
        /// `name -> handle` for children this instance started.
        ///
        /// Not a map, because it holds at most as many entries as the
        /// operator granted names — a handful — and a `Vec` keeps the
        /// listing in start order without a second structure.
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

    /// A refusal for a name the operator did not grant, naming the ones
    /// they did — a model that cannot see the alternatives cannot correct
    /// itself, and would either give up or guess again.
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
        // Starting twice is the model losing track, not an error worth
        // failing a turn over — and a second child under the same name
        // would leave the first unreachable and unkillable by name.
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
        // Both, always. An empty read does not mean the child is finished,
        // and a caller told only one of these either spins or stops early.
        serde_json::json!({
            "name": name,
            "output": out,
            "running": host_process::is_running(child),
        })
        .to_string()
    }

    /// Stop a child this session started. Idempotent.
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

    /// Not started *here*: a granted name nobody started reads differently
    /// from a name nobody granted, and the model's next move differs too.
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
        /// The children are the host's to reap. It kills what is left when
        /// this instance stops, which is the guarantee that holds whether
        /// or not a model remembered to stop anything (#109).
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
            // Every other op is about one child, and a missing name is the
            // model's mistake rather than a refusal — there is nothing to
            // tell it about a child it did not name.
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
