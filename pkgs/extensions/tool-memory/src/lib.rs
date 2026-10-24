//! `tool-memory`: facts shared across sessions in a run; `memory` holds the logic, this file the wasm32 glue.

/// The memory and its operations.
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

    const NAMESPACE: &str = "memory";
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
                    let rendered = memory.render();
                    if host_storage::set(NAMESPACE, KEY, &rendered).is_err() {
                        log(LogLevel::Warn, "the memory could not be written");
                        return Err(ToolError::Backend);
                    }
                    Ok(result)
                }
                Err(err) => {
                    log(LogLevel::Info, &format!("operation refused: {err}"));
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
