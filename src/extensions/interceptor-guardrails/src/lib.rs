//! `interceptor-guardrails` — content policy, as rules an operator writes.
//!
//! The interceptor set covers routing, context, tool choice and permission;
//! nothing inspected **content**. This one does: patterns from `config.yaml`
//! decide whether a tool argument is refused, put to the user, or allowed.
//!
//! Rules are data, never code — see [`rules`] for the shape and for why the
//! engine's linear-time guarantee is the point rather than a limitation.
//!
//! ## The four phases
//!
//! - `tool-call` — arguments matching a rule `block` with the rule's reason, or `ask`.
//! - `after-response` — the model's raw output: redact, or refuse the turn.
//! - `finalize` — the assembled answer, the last point before the transcript.
//! - `select-context` — messages on their way out, the only place matching text
//!   can be kept from leaving the machine at all.
//!
//! ## Off unless configured
//!
//! No rules means every call proceeds untouched. A content filter nobody asked
//! for is a surprise, so absence of configuration is absence of opinion.
//!
//! The rules are pure Rust with no WIT dependency, so they are unit-tested
//! natively; the glue compiles for `wasm32` only.

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
        /// The rule set, read once from `host-config` at `init`.
        ///
        /// Held as a `Result` rather than resolved to a default: a malformed
        /// rule must keep failing every dispatch, not decay into "no rules".
        static RULES: RefCell<Result<Rules, RuleError>> = RefCell::new(Ok(Rules::default()));
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-guardrails", message, &[]);
    }

    /// Read and cache the rule set from this extension's `config.yaml` section.
    ///
    /// An unreadable section reads as empty — that is the same state as being
    /// unconfigured. A section that *is* readable but holds a broken rule is
    /// kept as the error, and every later dispatch reports it.
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

    /// The confirmation to put to the driver when a rule asks rather than blocks.
    ///
    /// The default is refusal, matching every other gate here: an unanswered
    /// question must not become approval.
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
            log(LogLevel::Info, "started; matching tool arguments");
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
                HookState::Finalize(answer) => finalize(answer),
                HookState::SelectContext(request) => select_context(request),
                _ => {
                    log(LogLevel::Error, "dispatched with an unsubscribed state");
                    Err(InterceptorError::InvalidState)
                }
            }
        }
    }

    /// Run `read` against the rule set, or fail if the rules are broken.
    ///
    /// The host fails closed at `tool-call` on `Err` and logs-and-proceeds
    /// elsewhere, which is exactly the severity each phase deserves — so every
    /// phase reports the same error and lets host policy grade it.
    fn with_rules<T>(read: impl FnOnce(&Rules) -> T) -> Result<T, InterceptorError> {
        RULES.with(|r| match &*r.borrow() {
            Err(_) => Err(InterceptorError::Internal),
            Ok(rules) => Ok(read(rules)),
        })
    }

    /// `tool-call` — refuse or confirm a call whose arguments match a rule.
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

    /// `after-response` — the model's raw output, before core parses it.
    ///
    /// Redacting here rewrites what the turn goes on to treat as the answer,
    /// which is why the same rules run again at `finalize`: text assembled
    /// after this point has not been through them.
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

    /// `finalize` — the assembled answer, before it reaches the transcript and
    /// the client. The last point at which a secret can be kept out of the log.
    fn finalize(answer: FinalAnswer) -> Result<Decision, InterceptorError> {
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

    /// `select-context` — the messages on their way to the provider.
    ///
    /// The only phase that can keep matching text from *leaving the machine*,
    /// so a blocking rule here refuses the request rather than sending a
    /// redacted version of it.
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
