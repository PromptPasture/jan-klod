//! `tool-ask-probe` — a tool that asks before it answers.
//!
//! Test instrument: the only component exporting `tool-askable`, so the asking path in `tool_host` is covered.
//! Also exports `tool-callable`, so a host without `tool-askable` can still call it.
//! Arguments: `{}` asks once; `forever: true` asks until the host's bound stops it; `skip: true` never asks.

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
            world: "tool-asking-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_askable::{Ask, Guest as ToolAskable, Step};
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    /// The question, fixed so a test can recognise it.
    const QUESTION: &str = "which colour?";
    /// What the host uses when nobody can be asked.
    const DEFAULT: &str = "blue";

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "tool-ask-probe", message, &[]);
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
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    fn flag(arguments: &str, name: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get(name).and_then(serde_json::Value::as_bool))
            .unwrap_or(false)
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "ask-probe".to_string(),
                description: "Asks a question, then answers with what it was told.".to_string(),
                arguments_schema: r#"{"type":"object","properties":{
"forever":{"type":"boolean"},"skip":{"type":"boolean"}}}"#
                    .to_string(),
            }
        }

        /// The path a host that never learned `tool-askable` takes. It
        /// cannot ask, so it says so rather than pretending to have asked.
        fn invoke(_arguments: String) -> Result<String, ToolError> {
            Ok(r#"{"asked":false,"answer":null}"#.to_string())
        }
    }

    impl ToolAskable for Component {
        fn invoke_asking(arguments: String, answer: Option<String>) -> Result<Step, ToolError> {
            if flag(&arguments, "skip") {
                return Ok(Step::Done(r#"{"asked":false,"answer":null}"#.to_string()));
            }
            if flag(&arguments, "forever") {
                return Ok(Step::Asking(Ask {
                    question: QUESTION.to_string(),
                    options: vec![],
                    default_answer: DEFAULT.to_string(),
                }));
            }
            Ok(answer.map_or_else(
                || {
                    Step::Asking(Ask {
                        question: QUESTION.to_string(),
                        options: vec!["red".to_string(), "blue".to_string()],
                        default_answer: DEFAULT.to_string(),
                    })
                },
                |answer| {
                    Step::Done(serde_json::json!({ "asked": true, "answer": answer }).to_string())
                },
            ))
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
