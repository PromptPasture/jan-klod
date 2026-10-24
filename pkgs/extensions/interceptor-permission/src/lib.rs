//! `interceptor-permission`: the default `tool-call` gate. Scope check first, then a read-only
//! allowlist (see [`rules`]); anything else asks the driver.
//! `always`/`never` answers are standing decisions per kind of action (`fs:write`), kept for the
//! session unless `persist: true`. Scope escapes are never remembered; unreadable state asks.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod rules;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::rules::{Answer, Policy, Verdict};
    use core::cell::RefCell;

    /// `host-storage` namespace holding this run's standing decisions.
    const NAMESPACE: &str = "permission";

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
        Phase, UserPrompt,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::host_storage;

    thread_local! {
        /// Resolved permission policy, read once from `host-config` at `init`.
        static POLICY: RefCell<Policy> = RefCell::new(Policy::default());
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-permission", message, &[]);
    }

    /// Read the policy from this extension's config section; missing keys use defaults.
    fn load_policy() {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section =
            serde_json::from_str::<serde_json::Value>(&raw).unwrap_or(serde_json::Value::Null);
        POLICY.with(|p| *p.borrow_mut() = Policy::from_config(&section));
    }

    /// Count a refusal of `key` and return the running total. The counter sits under a distinct
    /// prefix so it can never be read as a verdict; an unreadable value counts as zero.
    fn count_refusal(key: &str) -> u32 {
        let counter = format!("refusals:{key}");
        let previous = host_storage::get(NAMESPACE, &counter)
            .ok()
            .and_then(|entry| entry.value.parse::<u32>().ok())
            .unwrap_or(0);
        let next = previous.saturating_add(1);
        let _ = host_storage::set(NAMESPACE, &counter, &next.to_string());
        next
    }

    /// Look up a standing decision for `key`. Any failure or non-verdict value returns `None`.
    fn recall(key: &str) -> Option<Verdict> {
        let entry = host_storage::get(NAMESPACE, key).ok()?;
        Verdict::parse(&entry.value)
    }

    /// Record a standing decision for this run. A failed write only costs a re-ask, so it is
    /// logged and ignored: failing to persist must never fail the security decision.
    fn remember(key: &str, verdict: Verdict) {
        if host_storage::set(NAMESPACE, key, verdict.as_str()).is_err() {
            log(
                LogLevel::Warn,
                &format!("could not record the decision for `{key}`; will ask again"),
            );
        } else {
            log(
                LogLevel::Info,
                &format!("`{key}` set to {} for this run", verdict.as_str()),
            );
        }
    }

    /// The confirmation to put to the driver. `always`/`never` are offered only with a scope.
    // `match` rather than `map_or_else`: the closure pair buries the subject of the question.
    #[allow(
        clippy::option_if_let_else,
        reason = "side-effecting arm reads worse as closures"
    )]
    fn prompt(tool: &str, arguments: &str, reason: &str, scope: Option<&str>) -> UserPrompt {
        let mut options = vec!["yes".to_string(), "no".to_string()];
        // Lead with what the call does: the user must see the change to weigh it.
        let what = crate::rules::summarise(tool, arguments);
        let question = match scope {
            Some(key) => {
                options.push("always".to_string());
                options.push("never".to_string());
                format!(
                    "Allow `{tool}` to {what}? ({reason}. \
                     `always`/`never` apply to `{key}` for the rest of this run.)"
                )
            }
            None => format!("Allow `{tool}` to {what}? ({reason})"),
        };
        UserPrompt {
            question,
            options,
            default_answer: "no".to_string(),
        }
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(
                LogLevel::Info,
                &format!("init id={} version={}", ctx.id, ctx.version),
            );
            load_policy();
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started; gating tool calls");
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
            vec![Phase::ToolCall]
        }

        #[allow(
            clippy::option_if_let_else,
            reason = "the scoped arm counts, logs and remembers before yielding a bool"
        )]
        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::ToolCall(call) = input.state else {
                log(LogLevel::Error, "dispatched with non-tool-call state");
                return Err(InterceptorError::InvalidState);
            };

            let Some(concern) = POLICY.with(|p| p.borrow().review(&call.name, &call.arguments))
            else {
                return Ok(Decision::Proceed);
            };
            let reason = concern.describe(&call.name);
            // No scope for an escape: that makes it un-rememberable, not merely un-remembered.
            let scope = concern
                .is_rememberable()
                .then(|| crate::rules::scope_key(&call.name, &call.arguments));

            match input.answer {
                None => {
                    // A standing decision answers for the user; unreadable state asks.
                    if let Some(key) = &scope {
                        match recall(key) {
                            Some(Verdict::Allow) => {
                                log(
                                    LogLevel::Info,
                                    &format!("`{key}` allowed by a standing decision"),
                                );
                                return Ok(Decision::Proceed);
                            }
                            Some(Verdict::Deny) => {
                                log(
                                    LogLevel::Info,
                                    &format!("`{key}` denied by a standing decision"),
                                );
                                return Ok(Decision::Block(BlockReason {
                                    message: crate::rules::denial_message(
                                        &crate::rules::summarise(&call.name, &call.arguments),
                                        true,
                                    ),
                                }));
                            }
                            None => {}
                        }
                    }
                    Ok(Decision::Ask(prompt(
                        &call.name,
                        &call.arguments,
                        &reason,
                        scope.as_deref(),
                    )))
                }
                // Resumed with the driver's answer; `always`/`never` also record a decision.
                Some(answer) => {
                    let answer = Answer::parse(&answer);
                    if let (Some(key), Some(verdict)) = (&scope, answer.standing()) {
                        remember(key, verdict);
                    }
                    if answer.approves() {
                        log(LogLevel::Info, &format!("tool `{}` approved", call.name));
                        Ok(Decision::Proceed)
                    } else {
                        // Past a few refusals, the refusal itself becomes the standing answer.
                        let standing = match &scope {
                            Some(key) => {
                                let count = count_refusal(key);
                                if count >= crate::rules::REFUSALS_BEFORE_STANDING_DENY {
                                    log(
                                        LogLevel::Warn,
                                        &format!(
                                            "`{key}` refused {count} times; refusing it \
                                             for the rest of this run without asking again"
                                        ),
                                    );
                                    remember(key, Verdict::Deny);
                                    true
                                } else {
                                    false
                                }
                            }
                            None => false,
                        };
                        log(LogLevel::Info, &format!("tool `{}` denied", call.name));
                        Ok(Decision::Block(BlockReason {
                            message: crate::rules::denial_message(
                                &crate::rules::summarise(&call.name, &call.arguments),
                                standing,
                            ),
                        }))
                    }
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
