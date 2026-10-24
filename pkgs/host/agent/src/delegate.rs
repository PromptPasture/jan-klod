//! Outbound `agent-*` ACP delegation through an injected [`AgentTransport`]; not wired into `Runtime::build_agent`.

use std::collections::HashMap;

use jk_protocol::intercept::ToolDefinition;
use jk_protocol::intercept::ToolInvoker;
use jk_protocol::turn::ToolCall;

/// The tool name the model uses to delegate.
pub const DELEGATE_TOOL: &str = "delegate";

/// Sends a task to a remote agent's ACP endpoint.
pub trait AgentTransport {
    /// Delegates `task` (with optional JSON `context`) to the agent at `endpoint`.
    ///
    /// # Errors
    /// Human-readable error if the agent is unreachable, rejects the task, or fails.
    fn delegate(&self, endpoint: &str, task: &str, context: Option<&str>)
        -> Result<String, String>;
}

/// [`ToolInvoker`] that services `delegate` calls.
pub struct AgentDelegate<T> {
    transport: T,
    agents: HashMap<String, String>,
}

impl<T: AgentTransport> AgentDelegate<T> {
    /// Delegate over `transport` to the named `agents` (name to endpoint).
    #[must_use]
    pub const fn new(transport: T, agents: HashMap<String, String>) -> Self {
        Self { transport, agents }
    }

    /// The `delegate` tool as advertised to the model.
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
    fn invoke(&mut self, call: &ToolCall) -> Option<jk_protocol::intercept::ToolInvocation> {
        if call.name != DELEGATE_TOOL {
            return None;
        }
        let args: serde_json::Value = serde_json::from_str(&call.arguments).ok()?;
        let agent = args.get("agent").and_then(serde_json::Value::as_str)?;
        let task = args.get("task").and_then(serde_json::Value::as_str)?;
        let context = args.get("context").and_then(serde_json::Value::as_str);

        let Some(endpoint) = self.agents.get(agent) else {
            return Some(jk_protocol::intercept::ToolInvocation {
                content: format!("unknown agent `{agent}`"),
                failed: true,
            });
        };
        Some(match self.transport.delegate(endpoint, task, context) {
            Ok(answer) => jk_protocol::intercept::ToolInvocation {
                content: answer,
                failed: false,
            },
            Err(err) => jk_protocol::intercept::ToolInvocation {
                content: format!("delegation to `{agent}` failed: {err}"),
                failed: true,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct StubTransport {
        seen: RefCell<Vec<(String, String)>>,
        reply: Result<String, String>,
    }

    impl AgentTransport for StubTransport {
        fn delegate(
            &self,
            endpoint: &str,
            task: &str,
            _context: Option<&str>,
        ) -> Result<String, String> {
            self.seen
                .borrow_mut()
                .push((endpoint.to_string(), task.to_string()));
            self.reply.clone()
        }
    }

    fn agents() -> HashMap<String, String> {
        HashMap::from([(
            "claude-code".to_string(),
            "http://acp.local/claude".to_string(),
        )])
    }

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn delegates_a_task_to_the_resolved_endpoint() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Ok("subtask done".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        let result = delegate.invoke(&call(
            "delegate",
            r#"{"agent":"claude-code","task":"write a test"}"#,
        ));
        let invocation = result.expect("delegate handled its own tool call");
        assert_eq!(invocation.content, "subtask done");
        assert!(!invocation.failed);
        assert_eq!(
            delegate.transport.seen.borrow().as_slice(),
            &[(
                "http://acp.local/claude".to_string(),
                "write a test".to_string()
            )]
        );
    }

    #[test]
    fn ignores_non_delegate_tool_calls() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Ok("x".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        assert_eq!(delegate.invoke(&call("web_search", "{}")), None);
    }

    #[test]
    fn reports_an_unknown_agent() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Ok("x".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        let invocation = delegate
            .invoke(&call("delegate", r#"{"agent":"nope","task":"t"}"#))
            .expect("delegate handled its own tool call");
        assert!(invocation.content.contains("unknown agent `nope`"));
        assert!(invocation.failed);
    }

    #[test]
    fn surfaces_a_transport_failure() {
        let transport = StubTransport {
            seen: RefCell::new(vec![]),
            reply: Err("unreachable".into()),
        };
        let mut delegate = AgentDelegate::new(transport, agents());
        let invocation = delegate
            .invoke(&call("delegate", r#"{"agent":"claude-code","task":"t"}"#))
            .expect("delegate handled its own tool call");
        assert!(invocation.content.contains("failed: unreachable"));
        assert!(invocation.failed);
    }

    #[test]
    fn tool_definition_names_the_delegate_tool() {
        let def = AgentDelegate::<StubTransport>::tool_definition();
        assert_eq!(def.name, "delegate");
        assert!(def.parameters_schema.contains("agent"));
    }
}
