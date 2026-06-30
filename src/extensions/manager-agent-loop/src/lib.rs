//! `manager-agent-loop` — the v0 agent loop, and the first guest that consumes
//! *other extensions* rather than only host capabilities.
//!
//! It imports `llm-provider` and `memory-store` (satisfied by the host routing
//! into the provider and store extensions — see the core's `route` module),
//! plus `host-log`, and exports the universal `extension-lifecycle` and the
//! minimal `agent-loop`. `run` does one turn: complete the prompt through the
//! routed provider, persist the prompt+response through the routed store, read it
//! back to confirm the round-trip, and return the completion text.
//!
//! This is deliberately the smallest loop that exercises inter-component routing
//! end to end; the routing/fallback/tool behaviour belongs to the future
//! `agent-manager`, not here.

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

/// Forward a line to the core's log pipeline, tagged with this component's name.
fn log(level: LogLevel, message: &str) {
    host_log::log(level, "manager-agent-loop", message, &[]);
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
        // 1. Complete through the routed provider, draining the stream to text.
        let request = CompletionRequest {
            model: String::new(), // provider falls back to its configured model
            messages: vec![Message {
                role: Role::User,
                content: prompt.clone(),
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
        let mut text = String::new();
        loop {
            match llm_provider::next_chunk(handle) {
                Some(CompletionChunk::TextDelta(delta)) => text.push_str(&delta),
                Some(CompletionChunk::Done(_)) => break,
                // Tool calls are out of scope for the v0 loop.
                Some(CompletionChunk::ToolCallRequest(_)) => {}
                None => break,
            }
        }
        llm_provider::close_stream(handle);

        // 2. Persist the turn through the routed store, then read it back to
        //    confirm the round-trip actually crossed both component boundaries.
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

        log(LogLevel::Info, "run complete");
        Ok(text)
    }
}

// The `export!` macro emits the component's `unsafe extern "C"` ABI shims at its
// call site, so scope the binding lints (incl. `unsafe_code`) to this glue too.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
