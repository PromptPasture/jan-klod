//! `tool-memory` — store and recall facts across sessions.
//!
//! A thin guest storing and recalling facts over the `host-storage` namespace
//! that already exists, with no new contract. Facts are run-scoped (survive
//! across sessions) and not session-scoped.
//!
//! [`memory`] is the whole tool and is pure. This file is the glue: read the
//! stored memory, apply one operation, write it back, return the result. It
//! compiles for `wasm32` only, which is why the logic lives next door where
//! the host target can test it.
//!
//! # Storage
//!
//! One namespace per component instance ("memory"), shared across all sessions
//! in the same run. The host prefixes the namespace at the component level
//! (becoming `ext/tool-memory/memory` in durable storage).

/// The memory and its operations, pure and host-testable.
pub mod memory;

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
    use bindings::jan_klod::interfaces::host_storage::{self, StoreError};

    use crate::memory::{Memory, MemoryError};

    /// Where the memory lives. One key stores all facts as a JSON array.
    const NAMESPACE: &str = "memory";
    /// The only key in the namespace.
    const KEY: &str = "facts";

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-memory", message, &[]);
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
                name: "tool-memory".to_string(),
                description:
                    "Store and recall facts across sessions. Use it to build up facts over time."
                        .to_string(),
                arguments_schema: r#"{"type":"object","required":["op"],"properties":{
"op":{"type":"string","enum":["store","recall","forget"]},
"key":{"type":"string","description":"the fact key, for store/recall-specific/forget"},
"value":{"type":"string","description":"the fact value, for op=store"}}}"#
                    .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            // A missing memory is an empty memory, not a failure: the first call
            // of a run is always a read of something that is not there.
            let stored = match host_storage::get(NAMESPACE, KEY) {
                Ok(entry) => Some(entry.value),
                Err(StoreError::NotFound) => None,
                Err(_) => {
                    log(LogLevel::Warn, "the memory could not be read");
                    return Err(ToolError::Backend);
                }
            };
            let mut memory = Memory::parse(stored.as_deref());

            match memory.apply(&arguments) {
                Ok(result) => {
                    // Write the updated memory back
                    let rendered = memory.render();
                    if host_storage::set(NAMESPACE, KEY, &rendered).is_err() {
                        log(LogLevel::Warn, "the memory could not be written");
                        return Err(ToolError::Backend);
                    }
                    Ok(result)
                }
                Err(err) => {
                    log(LogLevel::Info, &format!("operation refused: {err}"));
                    // Map memory errors to tool errors
                    match err {
                        MemoryError::BadArguments(_)
                        | MemoryError::MissingKey
                        | MemoryError::InvalidJson => Err(ToolError::InvalidArguments),
                        MemoryError::NotFound(_) => Err(ToolError::ExecutionFailed),
                    }
                }
            }
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
