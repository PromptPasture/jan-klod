//! `interceptor-intent-router`: default `before-loop` interceptor; `simple` prompts are answered inline, `agentic` proceeds.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod router;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::router::{self, Intent};

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
        BlockReason, Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError,
        Phase,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_provider::{
        self, CompletionChunk, CompletionRequest, Message, Role,
    };

    const CLASSIFIER_SYSTEM: &str = "You are an intent classifier. Reply with exactly one word. \
        Answer 'simple' when the message is a greeting, acknowledgement, or a single-fact question \
        answerable directly. Answer 'agentic' when it needs tools, planning, or multiple steps.";

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-intent-router", message, &[]);
    }

    /// Any provider failure defaults to [`Intent::Agentic`].
    fn llm_classify(prompt: &str) -> Intent {
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
                    content: prompt.to_string(),
                    tool_call_id: None,
                },
            ],
            tools: vec![],
            grammar: Some(router::CLASSIFIER_GRAMMAR.to_string()),
            max_tokens: Some(4),
            temperature: Some(0.0),
        };
        let Ok(handle) = llm_provider::complete(&request) else {
            log(
                LogLevel::Warn,
                "intent classifier call failed; defaulting to agentic",
            );
            return Intent::Agentic;
        };
        let text = drain_text(handle);
        router::parse_intent(&text)
    }

    /// Tool-call chunks are ignored; the classifier never emits them.
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
            log(LogLevel::Info, "started; classifying intent at before-loop");
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
            vec![Phase::BeforeLoop]
        }

        /// Any other state is a host dispatch error (`invalid-state`).
        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::BeforeLoop(turn) = input.state else {
                log(LogLevel::Error, "dispatched with non-before-loop state");
                return Err(InterceptorError::InvalidState);
            };
            match router::classify(&turn.user_message, llm_classify) {
                Intent::Simple => {
                    log(
                        LogLevel::Info,
                        "intent=simple; blocking agentic loop (answer inline)",
                    );
                    Ok(Decision::Block(BlockReason {
                        message: "intent=simple: answerable inline without the agentic loop"
                            .to_string(),
                    }))
                }
                Intent::Agentic => {
                    log(LogLevel::Info, "intent=agentic; proceeding into the loop");
                    Ok(Decision::Proceed)
                }
            }
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
