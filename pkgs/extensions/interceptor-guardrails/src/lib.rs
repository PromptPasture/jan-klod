//! `interceptor-guardrails`: operator-written content rules that block, ask about, or redact text.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod rules;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::rules::{Act, RuleError, Rules, TextVerdict};
    use core::cell::RefCell;

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
        BlockReason, Decision, FinalAnswer, Guest as Interceptor, HookState, InterceptInput,
        InterceptorError, Message, PendingRequest, Phase, RawResponse, ToolCall, UserPrompt,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

    thread_local! {
        /// A `Result`, so a malformed rule keeps failing every dispatch rather than becoming "no rules".
        static RULES: RefCell<Result<Rules, RuleError>> = RefCell::new(Ok(Rules::default()));
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-guardrails", message, &[]);
    }

    /// An unreadable section reads as unconfigured; a broken rule is kept and reported on every dispatch.
    fn load_rules() {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section =
            serde_json::from_str::<serde_json::Value>(&raw).unwrap_or(serde_json::Value::Null);
        let loaded = Rules::from_config(&section);
        if let Err(error) = &loaded {
            log(
                LogLevel::Error,
                &format!(
                    "refusing to run with a broken rule: {}. Tool calls will be \
                     blocked until `config.yaml` is fixed.",
                    error.describe()
                ),
            );
        }
        RULES.with(|r| *r.borrow_mut() = loaded);
    }

    /// Defaults to refusal: an unanswered question must not become approval.
    fn prompt(tool: &str, reason: &str) -> UserPrompt {
        UserPrompt {
            question: format!("Allow `{tool}`? (a guardrail matched: {reason})"),
            options: vec!["yes".to_owned(), "no".to_owned()],
            default_answer: "no".to_owned(),
        }
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(
                LogLevel::Info,
                &format!("init id={} version={}", ctx.id, ctx.version),
            );
            load_rules();
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started; matching content");
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
            vec![
                Phase::SelectContext,
                Phase::AfterResponse,
                Phase::ToolCall,
                Phase::Finalize,
            ]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            match input.state {
                HookState::ToolCall(call) => tool_call(&call, input.answer.as_deref()),
                HookState::AfterResponse(response) => after_response(response),
                HookState::Finalize(answer) => finalize(&answer),
                HookState::SelectContext(request) => select_context(request),
                _ => {
                    log(LogLevel::Error, "dispatched with an unsubscribed state");
                    Err(InterceptorError::InvalidState)
                }
            }
        }
    }

    /// Every phase reports the same error; host policy grades it (fail closed at `tool-call`).
    fn with_rules<T>(read: impl FnOnce(&Rules) -> T) -> Result<T, InterceptorError> {
        RULES.with(|r| {
            (*r.borrow())
                .as_ref()
                .map_or_else(|_| Err(InterceptorError::Internal), |rules| Ok(read(rules)))
        })
    }

    fn tool_call(call: &ToolCall, answer: Option<&str>) -> Result<Decision, InterceptorError> {
        let Some(verdict) =
            with_rules(|rules| rules.review_tool_call(&call.name, &call.arguments))?
        else {
            return Ok(Decision::Proceed);
        };

        match (verdict.act, answer) {
            (Act::Block, _) => {
                log(
                    LogLevel::Info,
                    &format!("`{}` blocked: {}", call.name, verdict.reason),
                );
                Ok(Decision::Block(BlockReason {
                    message: format!(
                        "`{}` was refused by a guardrail: {}",
                        call.name, verdict.reason
                    ),
                }))
            }
            (Act::Ask, None) => Ok(Decision::Ask(prompt(&call.name, &verdict.reason))),
            (Act::Ask, Some(answer)) => {
                if answer.trim().eq_ignore_ascii_case("yes") {
                    log(
                        LogLevel::Info,
                        &format!("`{}` allowed by the user", call.name),
                    );
                    Ok(Decision::Proceed)
                } else {
                    Ok(Decision::Block(BlockReason {
                        message: format!("`{}` was refused: {}", call.name, verdict.reason),
                    }))
                }
            }
        }
    }

    /// Redaction here is repeated at `finalize`, since later-assembled text has not passed these rules.
    fn after_response(response: RawResponse) -> Result<Decision, InterceptorError> {
        Ok(
            match with_rules(|rules| rules.review_text(&response.text))? {
                None => Decision::Proceed,
                Some(TextVerdict::Redacted(text)) => {
                    log(LogLevel::Info, "redacted the model's response");
                    Decision::Replace(HookState::AfterResponse(RawResponse { text, ..response }))
                }
                Some(TextVerdict::Blocked(reason)) => {
                    log(LogLevel::Warn, &format!("response refused: {reason}"));
                    Decision::Block(BlockReason {
                        message: format!("the response was refused by a guardrail: {reason}"),
                    })
                }
            },
        )
    }

    /// Last chance to keep a secret out of the transcript.
    fn finalize(answer: &FinalAnswer) -> Result<Decision, InterceptorError> {
        Ok(match with_rules(|rules| rules.review_text(&answer.text))? {
            None => Decision::Proceed,
            Some(TextVerdict::Redacted(text)) => {
                log(LogLevel::Info, "redacted the final answer");
                Decision::Replace(HookState::Finalize(FinalAnswer { text }))
            }
            Some(TextVerdict::Blocked(reason)) => {
                log(LogLevel::Warn, &format!("final answer refused: {reason}"));
                Decision::Block(BlockReason {
                    message: format!("the answer was refused by a guardrail: {reason}"),
                })
            }
        })
    }

    /// A block refuses the request outright, so matching text never leaves the machine.
    fn select_context(request: PendingRequest) -> Result<Decision, InterceptorError> {
        let reviewed = with_rules(|rules| {
            request
                .messages
                .iter()
                .map(|message| rules.review_text(&message.content))
                .collect::<Vec<_>>()
        })?;

        if let Some(TextVerdict::Blocked(reason)) = reviewed
            .iter()
            .find(|verdict| matches!(verdict, Some(TextVerdict::Blocked(_))))
            .and_then(Clone::clone)
        {
            log(LogLevel::Warn, &format!("context refused: {reason}"));
            return Ok(Decision::Block(BlockReason {
                message: format!("the request was refused by a guardrail: {reason}"),
            }));
        }

        if reviewed.iter().all(Option::is_none) {
            return Ok(Decision::Proceed);
        }

        let messages = request
            .messages
            .into_iter()
            .zip(reviewed)
            .map(|(message, verdict)| match verdict {
                Some(TextVerdict::Redacted(content)) => Message { content, ..message },
                _ => message,
            })
            .collect();
        log(LogLevel::Info, "redacted the outbound context");
        Ok(Decision::Replace(HookState::SelectContext(
            PendingRequest {
                messages,
                ..request
            },
        )))
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
