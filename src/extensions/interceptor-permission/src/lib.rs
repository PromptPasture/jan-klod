//! `interceptor-permission` — the default `tool-call` gate.
//!
//! A thin, single-rule permission interceptor: when a tool call looks dangerous
//! ([`rules::is_dangerous`]), it returns [`Decision::Ask`] so the loop's driver
//! confirms with the user; on the answer it [`Decision::Proceed`]s or
//! [`Decision::Block`]s. Ordinary calls proceed untouched.
//!
//! This exercises two mechanisms the loop relies on: the `ask` round-trip (the
//! host re-invokes `intercept` with `answer` set) and the fail-closed-at-tool-call
//! policy (a trap here is treated as a block by the core dispatcher).
//!
//! The rule is pure Rust with no WIT dependency, so it is unit-tested natively
//! (`cargo test`); the Component-Model glue below only compiles for `wasm32`.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod rules;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::rules;

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod bindings {
        wit_bindgen::generate!({
            world: "interceptor-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::interceptor::{
        BlockReason, Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError,
        Phase, UserPrompt,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-permission", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started; gating tool calls");
            Ok(())
        }
        fn stop() {
            log(LogLevel::Info, "stopping");
        }
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl Interceptor for Component {
        fn subscribed_phases() -> Vec<Phase> {
            vec![Phase::ToolCall]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::ToolCall(call) = input.state else {
                log(LogLevel::Error, "dispatched with non-tool-call state");
                return Err(InterceptorError::InvalidState);
            };

            if !rules::is_dangerous(&call.name) {
                return Ok(Decision::Proceed);
            }

            match input.answer {
                // First pass on a dangerous call: ask the driver to confirm.
                None => Ok(Decision::Ask(UserPrompt {
                    question: format!("Allow potentially dangerous tool `{}`?", call.name),
                    options: vec!["yes".to_string(), "no".to_string()],
                    default_answer: "no".to_string(),
                })),
                // Resumed with the driver's answer: proceed only on an explicit yes.
                Some(answer) => {
                    if rules::is_affirmative(&answer) {
                        log(LogLevel::Info, &format!("tool `{}` approved", call.name));
                        Ok(Decision::Proceed)
                    } else {
                        log(LogLevel::Info, &format!("tool `{}` denied", call.name));
                        Ok(Decision::Block(BlockReason {
                            message: format!("tool `{}` denied by user", call.name),
                        }))
                    }
                }
            }
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
