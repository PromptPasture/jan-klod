//! `interceptor-tool-selector` — the default `select-tools` interceptor.
//!
//! Fills `pending-request.tools` with the active tool set the host advertises via
//! `host-config` (`tools` — a JSON array of `{name, description, parameters-schema}`,
//! served from the loop's `ToolFleet`). If none are advertised it proceeds
//! unchanged. Per-step narrowing is a later refinement.
//!
//! Entirely Component-Model glue, so the crate only compiles for `wasm32`; on the
//! host target it builds as an empty lib.

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
            world: "interceptor-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::interceptor::{
        Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError, Phase,
        ToolDefinition,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-tool-selector", message, &[]);
    }

    /// The advertised tool set, read from the `tools` config key (a JSON array).
    fn advertised_tools() -> Vec<ToolDefinition> {
        let Ok(raw) = host_config::get("tools") else {
            return Vec::new();
        };
        let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&raw)
        else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|tool| {
                Some(ToolDefinition {
                    name: tool.get("name")?.as_str()?.to_string(),
                    description: tool
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    parameters_schema: tool
                        .get("parameters-schema")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("{}")
                        .to_string(),
                })
            })
            .collect()
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
            let HookState::SelectTools(mut request) = input.state else {
                log(LogLevel::Error, "dispatched with non-select-tools state");
                return Err(InterceptorError::InvalidState);
            };
            let tools = advertised_tools();
            if tools.is_empty() {
                return Ok(Decision::Proceed);
            }
            log(
                LogLevel::Info,
                &format!("select-tools: advertising {} tool(s)", tools.len()),
            );
            request.tools = tools;
            Ok(Decision::Replace(HookState::SelectTools(request)))
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
