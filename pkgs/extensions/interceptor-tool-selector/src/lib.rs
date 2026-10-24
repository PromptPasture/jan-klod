//! Default `select-tools` interceptor: cuts the advertised tools to the ones this turn needs.
//!
//! Unbounded by default. Component-Model glue only; the rules live in [`rank`].

/// Ranking of tool descriptions against the user's request.
pub mod rank;

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
        Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError,
        PendingRequest, Phase, ToolDefinition,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_types::Role;

    use crate::rank::{select, Cut, Doc};

    /// `max-tools` when unset: no bound. Hiding a tool is lossy and unrecoverable, so operators opt in.
    pub const DEFAULT_MAX_TOOLS: usize = 0;
    /// `min-tools` when unset: the floor a bounded fleet still advertises.
    pub const DEFAULT_MIN_TOOLS: usize = 3;

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-tool-selector", message, &[]);
    }

    fn number(key: &str, fallback: usize) -> usize {
        host_config::get(key)
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(fallback)
    }

    fn joined(request: &PendingRequest, wanted: impl Fn(&Role) -> bool) -> String {
        request
            .messages
            .iter()
            .filter(|message| wanted(&message.role))
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Tools from the `tools` config key; empty when absent or malformed.
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
            let docs: Vec<Doc> = tools
                .iter()
                .map(|tool| Doc::new(&tool.name, &tool.description))
                .collect();
            // Only the user's text ranks the fleet; a tool named anywhere in the conversation stays.
            let asked = joined(&request, |role| matches!(role, Role::User));
            let conversation = joined(&request, |_| true);
            let keep = select(
                &docs,
                &asked,
                &conversation,
                Cut {
                    most: number("max-tools", DEFAULT_MAX_TOOLS),
                    least: number("min-tools", DEFAULT_MIN_TOOLS),
                },
            );
            log(
                LogLevel::Info,
                &format!(
                    "select-tools: advertising {} of {} tool(s)",
                    keep.len(),
                    tools.len()
                ),
            );
            request.tools = tools
                .into_iter()
                .enumerate()
                .filter(|(index, _)| keep.contains(index))
                .map(|(_, tool)| tool)
                .collect();
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
