//! `tool-ask-probe` — a tool that asks before it answers.
//!
//! Test instrument, in the shape of `tool-escape-probe`: a component that
//! exists to exercise a boundary rather than to be useful. Nothing else
//! exports `tool-askable`, so without this the whole asking path in
//! `tool_host` has no coverage at all (#216).
//!
//! It exports both interfaces on purpose. `tool-callable` is what a host
//! that has not learned `tool-askable` will call, and a tool that answered
//! only one of them would be untestable from the other side.
//!
//! Three behaviours, chosen by the arguments:
//!
//! * `{}` — ask once, then answer with whatever came back. The ordinary
//!   case.
//! * `{"forever": true}` — ask every time, never finish. The host's bound
//!   is the only thing that ends this, which is what makes the bound worth
//!   having.
//! * `{"skip": true}` — answer without asking, proving the interface does
//!   not force a question.
//!
//! Component-Model glue only, so it compiles for `wasm32` only.

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

    /// Whether `arguments` set a boolean flag.
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
            // Asking again even with an answer in hand: only the host's
            // bound stops this, which is the point of the case.
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
