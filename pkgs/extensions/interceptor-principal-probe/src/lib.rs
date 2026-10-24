//! Probe: logs the optional principal at the `before-loop` phase.
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
    Decision, Guest as Interceptor, InterceptInput, InterceptorError, Phase,
};
use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

struct Component;

impl Lifecycle for Component {
    fn init(_ctx: ExtensionContext) -> Result<(), String> {
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

impl Interceptor for Component {
    fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
        if input.phase != Phase::BeforeLoop {
            return Ok(Decision::Proceed);
        }

        if let bindings::exports::jan_klod::interfaces::interceptor::HookState::BeforeLoop(
            user_turn,
        ) = &input.state
        {
            if let Some(ref principal) = user_turn.principal {
                let msg = format!("Principal is '{principal}'");
                host_log::log(LogLevel::Info, "interceptor-principal-probe", &msg, &[]);
            } else {
                host_log::log(
                    LogLevel::Info,
                    "interceptor-principal-probe",
                    "Principal is None",
                    &[],
                );
            }
        }

        Ok(Decision::Proceed)
    }

    fn subscribed_phases() -> Vec<Phase> {
        vec![Phase::BeforeLoop]
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
