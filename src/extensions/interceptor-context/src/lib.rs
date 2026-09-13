//! Default `select-context` interceptor.
//!
//! Trims conversation to model budget (char/4 estimate + sliding window).
//! Keeps system messages and recent turns. Reads budget from `host-config`
//! `context-tokens`, defaults to [`component::DEFAULT_BUDGET`].
//!
//! Trim logic is pure Rust, unit-tested natively; Component-Model glue
//! compiles for `wasm32` only.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod context;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::context;

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
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
        Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError, Phase,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_types::Role;

    /// Default token budget when `context-tokens` not configured.
    pub const DEFAULT_BUDGET: usize = 8192;

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-context", message, &[]);
    }

    /// Token budget for trimming: from `host-config` or default.
    fn budget() -> usize {
        host_config::get("context-tokens")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_BUDGET)
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
            log(
                LogLevel::Info,
                "started; trimming context to the model budget",
            );
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
            vec![Phase::SelectContext]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::SelectContext(mut request) = input.state else {
                log(LogLevel::Error, "dispatched with non-select-context state");
                return Err(InterceptorError::InvalidState);
            };

            let tokens: Vec<usize> = request
                .messages
                .iter()
                .map(|m| context::estimate_tokens(m.content.len()))
                .collect();
            let is_system: Vec<bool> = request
                .messages
                .iter()
                .map(|m| matches!(m.role, Role::System))
                .collect();

            let keep = context::keep_indices(&tokens, &is_system, budget());
            if keep.len() == request.messages.len() {
                return Ok(Decision::Proceed);
            }

            let dropped = request.messages.len() - keep.len();
            let trimmed: Vec<_> = keep.iter().map(|&i| request.messages[i].clone()).collect();
            request.messages = trimmed;
            log(
                LogLevel::Info,
                &format!("trimmed {dropped} message(s) to fit the budget"),
            );
            Ok(Decision::Replace(HookState::SelectContext(request)))
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
