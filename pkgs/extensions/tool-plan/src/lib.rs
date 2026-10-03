//! `tool-plan` — the session's plan, as something the model can write down.
//!
//! Working memory for a model too small to keep its intent in context. The
//! reasoning and the survey it came from are on
//! [#215](https://github.com/PromptPasture/jan-klod/issues/215); the short
//! version is that `docs/concepts/small-model-harness.md` lists this as
//! mitigation 3 and had nothing under it.
//!
//! [`plan`] is the whole tool and is pure. This file is the glue: read the
//! stored plan, apply one operation, write it back, return the result. It
//! compiles for `wasm32` only, which is why the logic lives next door where
//! the host target can test it.
//!
//! # Storage
//!
//! One namespace, one key. The instance must be configured `scope: session`
//! or two sessions share a plan — the host enforces the scoping, so this
//! guest holds no session id and cannot get the isolation wrong.

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
    // Named, not a glob. `clippy::wildcard-imports` is denied workspace-wide,
    // so a `::*` here would make the generated crate fail the repository's own
    // lints before its author had written a line — and the trait is aliased
    // because wit-bindgen calls every exported interface's trait `Guest`,
    // including the lifecycle one above.
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::host_storage::{self, StoreError};

    use crate::plan::Plan;

    /// Where the plan lives. One key, rewritten whole: a plan is small and
    /// the alternative is a merge nobody asked for.
    const NAMESPACE: &str = "plan";
    /// The only key in it.
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
            // A missing plan is an empty plan, not a failure: the first call
            // of a session is always a read of something that is not there.
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
                // The reason reaches the model through the log rather than
                // the error, because `tool-error` carries no message. A
                // refusal it cannot read is one it will repeat.
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
