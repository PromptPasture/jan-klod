//! `interceptor-context` — the default `select-context` interceptor.
//!
//! Trims the assembled conversation to the model's token budget before the
//! completion is issued: a char/4 estimate + a sliding window that keeps system
//! messages and the most recent turns ([`context`]). If nothing needs dropping it
//! proceeds; otherwise it replaces the request with the trimmed message list.
//!
//! The budget is read from `host-config` (`context-tokens`), defaulting to
//! [`component::DEFAULT_BUDGET`]. Summarising dropped history is a later
//! refinement behind this same seam.
//!
//! The trim logic is pure Rust with no WIT dependency, so it is unit-tested
//! natively (`cargo test`); the Component-Model glue only compiles for `wasm32`.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod context;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::context;

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
        Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError, Phase,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_types::Role;

    /// Token budget used when `host-config` supplies no `context-tokens` key.
    pub const DEFAULT_BUDGET: usize = 8192;

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-context", message, &[]);
    }

    /// The token budget for trimming: `host-config` `context-tokens`, else default.
    fn budget() -> usize {
        host_config::get("context-tokens")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_BUDGET)
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started; trimming context to the model budget");
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
            log(LogLevel::Info, &format!("trimmed {dropped} message(s) to fit the budget"));
            Ok(Decision::Replace(HookState::SelectContext(request)))
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
