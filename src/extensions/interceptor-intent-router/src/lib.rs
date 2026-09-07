//! `interceptor-intent-router` — the default `before-loop` interceptor.
//!
//! Every prompt is classified `simple` vs `agentic` before the agentic loop
//! runs. It exports the one generic `interceptor` interface
//! ([`wit/interceptor.wit`]) and subscribes to a single phase, `before-loop`:
//!
//! - `simple` (greeting, ack, single-fact question) → [`Decision::Block`],
//!   short-circuiting the agentic loop; the core answers inline.
//! - `agentic` (tools, planning, multi-step) → [`Decision::Proceed`], letting the
//!   loop run.
//!
//! The classification is the layered router ([`router`]): a pure-Rust language
//! gate + English heuristics settle the obvious cases with no model call;
//! anything else goes to a single constrained-decoding call on the routed
//! `llm-provider` import. `agentic` is the safe default — a prompt we cannot
//! classify never wrongly skips the loop.
//!
//! The router is pure Rust with no WIT dependency, so it is unit-tested natively
//! (`cargo test`); the Component-Model glue below only compiles for `wasm32`,
//! where the real provider-backed classifier is supplied.

// The router's only non-test consumer is the wasm32 `component` below. On other
// targets (a host `cargo check`/`cargo test`) that consumer is cfg'd out, so its
// items read as dead there — a target-conditional false positive. Real dead-code
// detection still applies for the wasm32 artifact, where the component uses them.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod router;

// Everything below is the Component-Model implementation: it depends on the
// generated WIT bindings, whose ABI shims only compile for `wasm32`. Gating it
// keeps a native `cargo test` (host target) building just `router` + its tests.
#[cfg(target_arch = "wasm32")]
mod component {
    use crate::router::{self, Intent};

    // Generated Component-Model bindings; lint exemptions (incl. the `unsafe` ABI
    // shims) scoped to the macro output.
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

    /// System prompt for the tier-3 intent classifier. Kept terse — the grammar
    /// ([`router::CLASSIFIER_GRAMMAR`]) is what actually constrains the output.
    const CLASSIFIER_SYSTEM: &str = "You are an intent classifier. Reply with exactly one word. \
        Answer 'simple' when the message is a greeting, acknowledgement, or a single-fact question \
        answerable directly. Answer 'agentic' when it needs tools, planning, or multiple steps.";

    /// Forward a line to the core's log pipeline, tagged with this component's name.
    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-intent-router", message, &[]);
    }

    /// Tier-3 classifier: one constrained-decoding call to the routed provider.
    /// On any provider failure it defaults to [`Intent::Agentic`] — never skip the
    /// loop for a prompt we could not classify.
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

    /// Drain a provider stream handle to its concatenated text, then close it.
    /// Tool-call chunks are ignored (the classifier never emits them).
    fn drain_text(handle: llm_provider::StreamHandle) -> String {
        let mut text = String::new();
        loop {
            match llm_provider::next_chunk(handle) {
                Some(CompletionChunk::TextDelta(delta)) => text.push_str(&delta),
                Some(CompletionChunk::ToolCallRequest(_)) => {}
                // Stream end or an exhausted/closed handle: stop draining.
                Some(CompletionChunk::Done(_)) | None => break,
            }
        }
        llm_provider::close_stream(handle);
        text
    }

    /// The single type implementing every interface `interceptor-world` exports.
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
        /// One phase only: decide, before the agentic loop runs, whether it needs
        /// to run at all.
        fn subscribed_phases() -> Vec<Phase> {
            vec![Phase::BeforeLoop]
        }

        /// Classify the user's turn. `simple` blocks the loop (the core answers
        /// inline); `agentic` proceeds into it. Any other phase state is a host
        /// dispatch error we could not act on — reported as `invalid-state`.
        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::BeforeLoop(turn) = input.state else {
                log(LogLevel::Error, "dispatched with non-before-loop state");
                return Err(InterceptorError::InvalidState);
            };
            // Cheap tiers (language + heuristics) settle obvious prompts with no
            // model call; otherwise the provider-backed classifier decides.
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

    // The `export!` macro emits the component's `unsafe extern "C"` ABI shims at
    // its call site, so scope the binding lints (incl. `unsafe_code`) to this glue.
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
