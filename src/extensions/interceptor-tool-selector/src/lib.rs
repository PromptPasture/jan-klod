//! `interceptor-tool-selector` — the default `select-tools` interceptor.
//!
//! Thin v1: a pass-through. The request-shaping pipeline reaches `select-tools`
//! with whatever tool set has been assembled; this interceptor exposes it as-is
//! and proceeds. Once tool sources (`tool-callable`, `mcp-registry`) are wired,
//! this is where the active tool set is assembled and (later) narrowed per step.
//!
//! Entirely Component-Model glue (no host-testable pure logic), so the crate only
//! compiles for `wasm32`; on the host target it builds as an empty lib.

#[cfg(target_arch = "wasm32")]
mod component {
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
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-tool-selector", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started; exposing the active tool set");
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
            vec![Phase::SelectTools]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::SelectTools(request) = input.state else {
                log(LogLevel::Error, "dispatched with non-select-tools state");
                return Err(InterceptorError::InvalidState);
            };
            // Pass-through: expose whatever tool set is already assembled. Tool
            // sources are wired later; for now proceed unchanged.
            log(
                LogLevel::Info,
                &format!("select-tools: {} tool(s) exposed", request.tools.len()),
            );
            Ok(Decision::Proceed)
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
