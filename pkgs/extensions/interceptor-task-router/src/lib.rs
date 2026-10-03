//! Default `select-model` interceptor — classify request to task type,
//! resolve routing table, set model. Pure Rust logic, unit-tested natively;
//! Component-Model glue for `wasm32` only.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod routing;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::routing;

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
    use bindings::jan_klod::interfaces::llm_provider::{
        self, CompletionChunk, CompletionRequest, Message, Role,
    };

    const CLASSIFIER_SYSTEM: &str = "You are a task classifier. Reply with exactly one task \
        label from the allowed set that best fits the user's request.";

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-task-router", message, &[]);
    }

    /// Collect stream text and close handle.
    fn drain_text(handle: llm_provider::StreamHandle) -> String {
        let mut text = String::new();
        loop {
            match llm_provider::next_chunk(handle) {
                Some(CompletionChunk::TextDelta(delta)) => text.push_str(&delta),
                Some(CompletionChunk::ToolCallRequest(_)) => {}
                Some(CompletionChunk::Done(_)) | None => break,
            }
        }
        llm_provider::close_stream(handle);
        text
    }

    /// Classify message into built-in task via constrained-decoding call.
    fn classify(user_message: &str) -> Option<String> {
        let grammar = routing::classifier_grammar(routing::BUILT_IN_TASKS);
        let request = CompletionRequest {
            model: String::new(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: CLASSIFIER_SYSTEM.to_string(),
                    tool_call_id: None,
                },
                Message {
                    role: Role::User,
                    content: user_message.to_string(),
                    tool_call_id: None,
                },
            ],
            tools: vec![],
            grammar: Some(grammar),
            max_tokens: Some(8),
            temperature: Some(0.0),
        };
        let handle = llm_provider::complete(&request).ok()?;
        let text = drain_text(handle);
        routing::parse_task(&text, routing::BUILT_IN_TASKS)
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
            log(LogLevel::Info, "started; routing tasks to models");
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
            vec![Phase::SelectModel]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::SelectModel(mut request) = input.state else {
                log(LogLevel::Error, "dispatched with non-select-model state");
                return Err(InterceptorError::InvalidState);
            };

            let Some(user_message) = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.content.clone())
            else {
                return Ok(Decision::Proceed);
            };

            let task = classify(&user_message).unwrap_or_else(|| routing::DEFAULT_TASK.to_string());

            // Resolve routing table entry.
            let Ok(raw) = host_config::get(&format!("routing.{task}")) else {
                log(
                    LogLevel::Info,
                    &format!("task={task}; no route configured; proceeding"),
                );
                return Ok(Decision::Proceed);
            };
            // host-config serves quoted JSON strings.
            let route = raw.trim().trim_matches('"');
            let Some(model) = routing::model_from_route(route) else {
                return Ok(Decision::Proceed);
            };

            if let Some(provider) = routing::unhonoured_provider(route) {
                // Log names the endpoint request won't reach (chain fixed at boot).
                log(
                    LogLevel::Warn,
                    &format!(
                        "route `{route}` names provider `{provider}` (unreachable in \
                         fallback chain). Use bare model name or reorder `providers:`."
                    ),
                );
            }
            log(LogLevel::Info, &format!("task={task} -> model={model}"));
            request.model = Some(model.to_string());
            Ok(Decision::Replace(HookState::SelectModel(request)))
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
