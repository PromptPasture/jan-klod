//! `interceptor-permission` — the default `tool-call` gate.
//!
//! Gates tool calls on two checks (see [`rules`]):
//!
//! - **Scope check**: any string argument contains an absolute path or a `..`
//!   traversal that would escape the workspace root.
//! - **Allowlist check**: the call is one of the read-only operations the
//!   operator named. Anything else is confirmed.
//!
//! Either condition returns [`Decision::Ask`] so the loop's driver confirms with
//! the user; on the answer it [`Decision::Proceed`]s or [`Decision::Block`]s.
//! Known read-only, in-scope calls proceed untouched.
//!
//! The allowlist replaced a denylist of high-risk verbs. A denylist can only name
//! the verbs someone thought of, and `tool-edit` arrived with the ops `view`,
//! `replace` and `insert` — none of them `write`, and `edit` is not `shell` — so
//! the tool that modifies files in place was ungated from the commit that added
//! it, with nothing failing to say so.
//!
//! ## Standing decisions ("always" / "never")
//!
//! A gate that asks the same question forty times is a gate people switch off,
//! and a switched-off gate protects nothing — so the confirmation can be answered
//! `always` or `never`, recorded per *kind of action* (`fs:write`, `shell:cargo`)
//! and consulted before asking again. Three properties keep that from eroding the
//! boundary:
//!
//! - **Run-scoped, never persisted.** Decisions live in `host-storage`, which is
//!   memory owned by this instance unless the operator grants `persist: true`.
//!   Restart the agent and it asks again — a permission boundary should not
//!   quietly become permanently open because of a click last week. This used to
//!   be true only because the host had no other option; it is now the default
//!   rather than the mechanism, and `storage_scope.rs` fails if it flips.
//! - **A scope escape is never remembered** ([`rules::Concern::is_rememberable`]).
//!   "Always allow writes" covers writing files, not writing `/etc/passwd`.
//! - **Unreadable state means ask.** A storage error or an unrecognised stored
//!   value falls back to the question, never to approval.
//!
//! The rules are pure Rust with no WIT dependency, so they are unit-tested
//! natively (`cargo test`); the Component-Model glue below only compiles for
//! `wasm32`.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod rules;

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::rules::{Answer, Policy, Verdict};
    use core::cell::RefCell;

    /// `host-storage` namespace holding this run's standing decisions.
    const NAMESPACE: &str = "permission";

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
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

    /// Read and cache the policy from this extension's `config.yaml` section. A
    /// missing/unreadable section or absent keys fall back to the built-in
    /// defaults (see [`Policy::from_config`]).
    fn load_policy() {
        let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        let section = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or(serde_json::Value::Null);
        POLICY.with(|p| *p.borrow_mut() = Policy::from_config(&section));
    }

    /// Look up a standing decision for `key`.
    ///
    /// A storage failure, a missing entry, or a value that is not a verdict all
    /// return `None` — the caller then asks. Nothing here can turn a broken read
    /// into an approval.
    fn recall(key: &str) -> Option<Verdict> {
        let entry = host_storage::get(NAMESPACE, key).ok()?;
        Verdict::parse(&entry.value)
    }

    /// Record a standing decision for the rest of this run.
    ///
    /// A failed write is logged and otherwise ignored: the user's call still takes
    /// effect for *this* invocation, they will simply be asked again next time.
    /// Failing to persist a convenience must never fail the security decision.
    fn remember(key: &str, verdict: Verdict) {
        if host_storage::set(NAMESPACE, key, verdict.as_str()).is_err() {
            log(LogLevel::Warn, &format!("could not record the decision for `{key}`; will ask again"));
        } else {
            log(LogLevel::Info, &format!("`{key}` set to {} for this run", verdict.as_str()));
        }
    }

    /// The confirmation to put to the driver. `always`/`never` are only offered
    /// when there is a scope to file them against.
    fn prompt(tool: &str, arguments: &str, reason: &str, scope: Option<&str>) -> UserPrompt {
        let mut options = vec!["yes".to_string(), "no".to_string()];
        // Lead with what the call *does*. "Allow tool `edit`? Reason: `edit` is
        // not a known read-only call" asks someone to approve a file
        // modification without naming the file or the change, and consent given
        // without the material facts is not consent — the only reason to stop
        // and ask is that a person can weigh this particular action.
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
        UserPrompt { question, options, default_answer: "no".to_string() }
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            log(LogLevel::Info, &format!("init id={} version={}", ctx.id, ctx.version));
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

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::ToolCall(call) = input.state else {
                log(LogLevel::Error, "dispatched with non-tool-call state");
                return Err(InterceptorError::InvalidState);
            };

            let Some(concern) =
                POLICY.with(|p| p.borrow().review(&call.name, &call.arguments))
            else {
                return Ok(Decision::Proceed);
            };
            let reason = concern.describe(&call.name);
            // Only a rememberable concern gets a scope; an escape has none, which
            // is what makes it un-rememberable rather than merely un-remembered.
            let scope = concern
                .is_rememberable()
                .then(|| crate::rules::scope_key(&call.name, &call.arguments));

            match input.answer {
                None => {
                    // A standing decision from earlier in this run answers for the
                    // user. Anything unreadable falls through to asking.
                    if let Some(key) = &scope {
                        match recall(key) {
                            Some(Verdict::Allow) => {
                                log(LogLevel::Info, &format!("`{key}` allowed by a standing decision"));
                                return Ok(Decision::Proceed);
                            }
                            Some(Verdict::Deny) => {
                                log(LogLevel::Info, &format!("`{key}` denied by a standing decision"));
                                return Ok(Decision::Block(BlockReason {
                                    message: format!("`{key}` was denied for this run"),
                                }));
                            }
                            None => {}
                        }
                    }
                    Ok(Decision::Ask(prompt(&call.name, &call.arguments, &reason, scope.as_deref())))
                }
                // Resumed with the driver's answer. `always`/`never` also record a
                // standing decision for this run before acting on it.
                Some(answer) => {
                    let answer = Answer::parse(&answer);
                    if let (Some(key), Some(verdict)) = (&scope, answer.standing()) {
                        remember(key, verdict);
                    }
                    if answer.approves() {
                        log(LogLevel::Info, &format!("tool `{}` approved", call.name));
                        Ok(Decision::Proceed)
                    } else {
                        log(LogLevel::Info, &format!("tool `{}` denied", call.name));
                        Ok(Decision::Block(BlockReason {
                            message: format!("tool `{}` denied by user", call.name),
                        }))
                    }
                }
            }
        }
    }

    #[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
