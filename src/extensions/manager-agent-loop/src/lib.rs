//! `manager-agent-loop` — the agent loop, and the first guest that consumes
//! *other extensions* rather than only host capabilities.
//!
//! It imports `llm-provider` and `memory-store` (satisfied by the host routing
//! into the provider and store extensions — see the core's `route` module),
//! plus `host-log`, and exports the universal `extension-lifecycle` and the
//! minimal `agent-loop`.
//!
//! Slice 2a adds the layered **intent router** ([`router`]): every prompt is
//! classified `simple` vs `agentic` before any work. `simple` is answered inline
//! with one completion; the `agentic` path is where the `ReAct` step controller
//! lands in Slice 2b — until then it falls back to the same one-shot turn.
//!
//! A turn: complete the prompt through the routed provider, persist the
//! prompt+response through the routed store, read it back to confirm the
//! round-trip, and return the completion text.
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
    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod bindings {
        wit_bindgen::generate!({
            world: "agent-loop-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::agent_loop::{AgentError, Guest as AgentLoop};
    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_provider::{
        self, CompletionChunk, CompletionRequest, Message, Role,
    };
    use bindings::jan_klod::interfaces::memory_store;

    /// Namespace the loop persists each turn under.
    const HISTORY_NS: &str = "agent.history";
    /// Key for the latest turn (v0 keeps just one).
    const TURN_KEY: &str = "turn";

    /// System prompt for the tier-3 intent classifier. Kept terse — the grammar
    /// ([`router::CLASSIFIER_GRAMMAR`]) is what actually constrains the output.
    const CLASSIFIER_SYSTEM: &str = "You are an intent classifier. Reply with exactly one word. \
        Answer 'simple' when the message is a greeting, acknowledgement, or a single-fact question \
        answerable directly. Answer 'agentic' when it needs tools, planning, or multiple steps.";

    /// Forward a line to the core's log pipeline, tagged with this component's name.
    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "manager-agent-loop", message, &[]);
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
            log(LogLevel::Warn, "intent classifier call failed; defaulting to agentic");
            return Intent::Agentic;
        };
        let text = drain_text(handle);
        router::parse_intent(&text)
    }

    /// Drain a provider stream handle to its concatenated text, then close it.
    /// Tool-call chunks are ignored (not used on the classify/one-shot paths).
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

    /// One completion + persisted turn: complete `prompt` through the routed
    /// provider, store the prompt+response through the routed store, read it back
    /// to confirm the round-trip crossed both component boundaries, and return the
    /// completion text.
    fn one_shot(prompt: &str) -> Result<String, AgentError> {
        let request = CompletionRequest {
            model: String::new(), // provider falls back to its configured model
            messages: vec![Message {
                role: Role::User,
                content: prompt.to_string(),
                tool_call_id: None,
            }],
            tools: vec![],
            grammar: None,
            max_tokens: Some(512),
            temperature: Some(0.0),
        };
        let handle = llm_provider::complete(&request).map_err(|err| {
            log(LogLevel::Error, &format!("provider failed: {err:?}"));
            AgentError::ProviderFailed
        })?;
        let text = drain_text(handle);

        let value = serde_json::json!({ "prompt": prompt, "response": text }).to_string();
        memory_store::set(HISTORY_NS, TURN_KEY, &value).map_err(|err| {
            log(LogLevel::Error, &format!("store set failed: {err:?}"));
            AgentError::StoreFailed
        })?;
        let stored = memory_store::get(HISTORY_NS, TURN_KEY).map_err(|err| {
            log(LogLevel::Error, &format!("store get failed: {err:?}"));
            AgentError::StoreFailed
        })?;
        if stored.value != value {
            log(LogLevel::Error, "store round-trip mismatch");
            return Err(AgentError::StoreFailed);
        }
        Ok(text)
    }

    /// The single type implementing every interface `agent-loop-world` exports.
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
            log(LogLevel::Info, "started; routing provider + store");
            Ok(())
        }

        fn stop() {
            log(LogLevel::Info, "stopping");
        }

        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl AgentLoop for Component {
        fn run(prompt: String) -> Result<String, AgentError> {
            // Gate 1: classify before doing any work. Cheap tiers (language +
            // heuristics) settle obvious prompts with no model call; otherwise the
            // provider-backed classifier decides.
            match router::classify(&prompt, llm_classify) {
                Intent::Simple => {
                    log(LogLevel::Info, "intent=simple; answering inline (no agent loop)");
                }
                Intent::Agentic => {
                    // The ReAct step controller lands in Slice 2b; until then an
                    // agentic prompt still runs the one-shot completion path.
                    log(
                        LogLevel::Info,
                        "intent=agentic; step controller pending (Slice 2b) — one-shot fallback",
                    );
                }
            }
            one_shot(&prompt)
        }
    }

    // The `export!` macro emits the component's `unsafe extern "C"` ABI shims at
    // its call site, so scope the binding lints (incl. `unsafe_code`) to this glue.
    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
