//! `interceptor-permission` — the default `tool-call` gate.
//!
//! Gates tool calls on three independent checks (see [`rules`]):
//!
//! - **Name check**: the tool name contains a high-risk verb (`shell`, `exec`, …).
//! - **Op check**: a benign-named multi-op tool selects a mutating operation via
//!   an `{"op":"write"}` argument (e.g. the unified `fs` tool).
//! - **Scope check**: any string argument contains an absolute path or a `..`
//!   traversal that would escape the workspace root.
//!
//! Either condition returns [`Decision::Ask`] so the loop's driver confirms with
//! the user; on the answer it [`Decision::Proceed`]s or [`Decision::Block`]s.
//! Ordinary, in-scope calls proceed untouched.
//!
//! The rules are pure Rust with no WIT dependency, so they are unit-tested
//! natively (`cargo test`); the Component-Model glue below only compiles for
//! `wasm32`.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod rules;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::rules::{self, Policy};
    use core::cell::RefCell;

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
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    thread_local! {
        /// Resolved permission policy, read once from `host-config` at `init`.
        static POLICY: RefCell<Policy> = RefCell::new(Policy::default());
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-permission", message, &[]);
    }

    /// Read and cache the policy from this extension's `config.yaml` section. A
    /// missing/unreadable section or absent keys fall back to the built-in
    /// defaults (see [`Policy::from_config`]).
    fn load_policy() {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or(serde_json::Value::Null);
        POLICY.with(|p| *p.borrow_mut() = Policy::from_config(&section));
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
            load_policy();
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

            let reason = POLICY.with(|p| {
                let policy = p.borrow();
                if policy.is_dangerous(&call.name) {
                    Some(format!("tool `{}` has a high-risk name", call.name))
                } else if policy.args_are_dangerous(&call.arguments) {
                    Some(format!("tool `{}` selects a high-risk operation", call.name))
                } else if policy.args_escape_scope(&call.arguments) {
                    Some(format!(
                        "tool `{}` arguments reference a path outside the workspace",
                        call.name
                    ))
                } else {
                    None
                }
            });

            let Some(reason) = reason else {
                return Ok(Decision::Proceed);
            };

            match input.answer {
                // First pass: ask the driver to confirm.
                None => Ok(Decision::Ask(UserPrompt {
                    question: format!("Allow tool `{}`? Reason: {reason}", call.name),
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
