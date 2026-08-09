//! `agent-*` ACP delegation — outbound (Phase 4 Slice 4c).
//!
//! Delegating a subtask to another AI agent is something the loop *calls*, like a
//! tool — so it plugs into the conductor's existing [`ToolInvoker`] seam rather
//! than a new mechanism. The model emits a `delegate` tool call
//! (`{ "agent", "task", "context"? }`); [`AgentDelegate`] resolves the agent to its
//! ACP endpoint and forwards the task over an injected [`AgentTransport`],
//! returning the remote agent's answer as the tool result. The ACP wire format
//! lives behind the transport, so the seam is unit-tested offline.
//!
//! **Inbound** delegation (core *called by* another ACP orchestrator) needs no new
//! code: it is the host-side REST surface (`jan_klod_core::serve`) — an orchestrator
//! `POST`s a task as a turn and reads the answer. The concrete ACP↔REST framing is
//! a thin adapter over that surface.
//!
//! # Not wired
//!
//! **Nothing constructs this.** `Runtime::build_agent` assembles providers, tools,
//! registries, and interceptors; the `agent` category is ranked for boot ordering
//! and never instantiated, and no `AgentDelegate` reaches the `CombinedFleet`. So a
//! model emitting a `delegate` tool call today gets "no tool named `delegate`".
//!
//! It is kept rather than deleted because it misleads nobody: it is host-side, so
//! no extension can import it and wait forever the way `host-event`'s removed
//! polling half could. What it needs is a transport and one line in `build_agent`,
//! and until it has them this notice is the honest state of it.

use std::collections::HashMap;

use crate::conductor::ToolInvoker;
use crate::intercept::{ToolCall, ToolDefinition};

/// The tool name the model uses to delegate to another agent.
pub const DELEGATE_TOOL: &str = "delegate";

/// Carries a task to a remote agent's ACP endpoint and returns its answer text.
/// Injected so the delegation seam is testable without a live agent; the concrete
/// ACP-over-HTTP client is one implementor.
pub trait AgentTransport {
    /// Delegate `task` (with optional JSON `context`) to the agent at `endpoint`.
    ///
    /// # Errors
    /// Returns a human-readable error if the remote agent is unreachable, rejects
    /// the task, times out, or speaks a bad protocol.
    fn delegate(&self, endpoint: &str, task: &str, context: Option<&str>) -> Result<String, String>;
}

/// A [`ToolInvoker`] that services `delegate` tool calls by forwarding to a remote
/// agent. Non-`delegate` calls are ignored (returns `None`, so it composes as the
/// delegation half of a larger tool set).
pub struct AgentDelegate<T> {
    transport: T,
    /// Agent id → ACP endpoint (from `config.yaml`'s `agent.*` instances).
    agents: HashMap<String, String>,
}

impl<T: AgentTransport> AgentDelegate<T> {
    /// Build a delegator over `transport` with the known `agents` (id → endpoint).
    #[must_use]
    pub const fn new(transport: T, agents: HashMap<String, String>) -> Self {
        Self { transport, agents }
    }

    /// The tool definition to advertise to the model (so it can choose to delegate).
    #[must_use]
    pub fn tool_definition() -> ToolDefinition {
        ToolDefinition {
            name: DELEGATE_TOOL.to_string(),
            description: "Delegate a self-contained subtask to another AI agent and \
                return its result."
                .to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "id of the agent to delegate to" },
                    "task": { "type": "string", "description": "the subtask, in natural language" },
                    "context": { "type": "string", "description": "optional JSON context" }
                },
                "required": ["agent", "task"]
            })
            .to_string(),
        }
    }
}

impl<T: AgentTransport> ToolInvoker for AgentDelegate<T> {
    fn invoke(&mut self, call: &ToolCall) -> Option<String> {
        if call.name != DELEGATE_TOOL {
            return None; // not ours — skip-if-absent
        }
        let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
        let agent = args.get("agent").and_then(serde_json::Value::as_str)?;
        let task = args.get("task").and_then(serde_json::Value::as_str)?;
        let context = args.get("context").and_then(serde_json::Value::as_str);

        let Some(endpoint) = self.agents.get(agent) else {
            return Some(format!("unknown agent `{agent}`"));
        };
        Some(match self.transport.delegate(endpoint, task, context) {
            Ok(result) => result,
            Err(err) => format!("delegation to `{agent}` failed: {err}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A transport that records what it was asked and returns a canned answer.
    struct StubTransport {
        seen: RefCell<Vec<(String, String)>>,
        reply: Result<String, String>,
    }

    impl AgentTransport for StubTransport {
        fn delegate(&self, endpoint: &str, task: &str, _context: Option<&str>) -> Result<String, String> {
            self.seen.borrow_mut().push((endpoint.to_string(), task.to_string()));
            self.reply.clone()
        }
    }

    fn agents() -> HashMap<String, String> {
        HashMap::from([("claude-code".to_string(), "http://acp.local/claude".to_string())])
    }

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "1".into(), name: name.into(), arguments: args.into() }
    }

    #[test]
    fn delegates_a_task_to_the_resolved_endpoint() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Ok("subtask done".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        let result =
            delegate.invoke(&call("delegate", r#"{"agent":"claude-code","task":"write a test"}"#));
        assert_eq!(result.as_deref(), Some("subtask done"));
        assert_eq!(
            delegate.transport.seen.borrow().as_slice(),
            &[("http://acp.local/claude".to_string(), "write a test".to_string())]
        );
    }

    #[test]
    fn ignores_non_delegate_tool_calls() {
        let transport = StubTransport { seen: RefCell::new(vec![]), reply: Ok("x".into()) };
        let mut delegate = AgentDelegate::new(transport, agents());
        assert_eq!(delegate.invoke(&call("web_search", "{}")), None);
    }

    #[test]
    fn reports_an_unknown_agent() {
        let transport = StubTransport { seen: RefCell::new(vec![]), reply: Ok("x".into()) };
        let mut delegate = AgentDelegate::new(transport, agents());
        let result = delegate.invoke(&call("delegate", r#"{"agent":"nope","task":"t"}"#));
        assert!(result.unwrap().contains("unknown agent `nope`"));
    }

    #[test]
    fn surfaces_a_transport_failure() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Err("unreachable".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        let result = delegate.invoke(&call("delegate", r#"{"agent":"claude-code","task":"t"}"#));
        assert!(result.unwrap().contains("failed: unreachable"));
    }

    #[test]
    fn tool_definition_names_the_delegate_tool() {
        let def = AgentDelegate::<StubTransport>::tool_definition();
        assert_eq!(def.name, "delegate");
        assert!(def.parameters_schema.contains("agent"));
    }
}
