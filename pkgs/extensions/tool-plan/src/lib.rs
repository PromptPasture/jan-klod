//! `tool-plan` — the session's plan, as something the model can write down.
//!
//! Working memory for a model too small to keep its intent in context.
//! [`plan`] holds the pure logic; this file is the wasm32-only glue.
//! The instance must be configured `scope: session`, so sessions do not share a plan.

/// The plan and its operations, pure and host-testable.
pub mod plan;

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

    use crate::plan::Plan;

    /// Where the plan lives. One key, rewritten whole: the plan is small.
    const NAMESPACE: &str = "plan";
    const KEY: &str = "steps";

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-plan", message, &[]);
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
                name: "plan".to_string(),
                description: "Keep a plan for this task. One change per call; \
                              every call returns the whole plan. Use it to \
                              record what you intend to do and what you have \
                              done, so you do not have to remember it."
                    .to_string(),
                arguments_schema: r#"{"type":"object","required":["op"],"properties":{
"op":{"type":"string","enum":["list","add","set-state","remove","clear"]},
"text":{"type":"string","description":"the step, for op=add"},
"id":{"type":"integer","description":"which step, for op=set-state and op=remove"},
"state":{"type":"string","enum":["todo","doing","done"],"description":"for op=set-state"}}}"#
                    .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let stored = match host_storage::get(NAMESPACE, KEY) {
                Ok(entry) => Some(entry.value),
                Err(StoreError::NotFound) => None,
                Err(_) => {
                    log(LogLevel::Warn, "the plan could not be read");
                    return Err(ToolError::Backend);
                }
            };
            let mut plan = Plan::parse(stored.as_deref());

            plan.apply(&arguments).map_err(|err| {
                log(LogLevel::Info, &format!("refused: {err}"));
                ToolError::InvalidArguments
            })?;

            let rendered = plan.render();
            if host_storage::set(NAMESPACE, KEY, &rendered).is_err() {
                log(LogLevel::Warn, "the plan could not be written");
                return Err(ToolError::Backend);
            }
            Ok(rendered)
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
