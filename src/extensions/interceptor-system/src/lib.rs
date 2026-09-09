//! `interceptor-system` — the standing instructions a turn runs under. Without
//! it, the model gets the raw conversation with no framing: it isn't told it's
//! an agent, that paths are workspace-relative, or that writes get confirmed —
//! for the small models this runtime targets, that framing is most of the
//! difference between a tool call and a paragraph describing one.
//!
//! ## Why an extension, and why `select-model`
//!
//! A system prompt is policy, so it lives in a swappable/reconfigurable guest
//! rather than core.
//!
//! It runs at `select-model` (not the more obvious `select-context`) because
//! that phase runs first: the prompt must already be in the message list when
//! `interceptor-context` measures the token budget, or those tokens go uncounted.
//!
//! ## Idempotence
//!
//! Prepends only when no system message is already present, so a turn that
//! already has one (driver-set, another interceptor, a replayed conversation)
//! doesn't accumulate copies.
//!
//! The assembly is pure Rust (unit-tested natively); the Component-Model glue
//! below only compiles for `wasm32`.

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod prompt {
    /// The built-in instructions, used when config supplies no `prompt`.
    ///
    /// Each line earns its place by naming something the runtime actually
    /// enforces, so the model's expectations match the sandbox's behaviour rather
    /// than being surprised by it: paths are jailed, writes are confirmed,
    /// partial edits are anchored, and output goes into the context budget.
    pub const DEFAULT: &str = "\
You are jan-klod, a coding agent working inside one workspace directory.

- Paths are workspace-relative. Absolute paths and `..` are refused by the \
sandbox, not merely discouraged.
- Use the tools rather than describing what you would do. `find` locates files by \
glob, `fs` reads and greps them (grep searches the whole tree by default), `edit` \
changes part of a file, `git` shows the repository state read-only.
- Prefer `edit` over rewriting a file: it anchors on the lines you saw, so a stale \
edit is rejected instead of applied to the wrong place.
- Writes and commands are confirmed with the user before they happen. Expect that, \
and do not plan around avoiding it.
- Tool output is capped and may say it was truncated. Narrow the query rather than \
assuming you saw everything.
- Answer concisely; the conversation shares the model's context window with the \
files you read.";

    /// The system text for this turn: the configured `prompt`, else [`DEFAULT`].
    ///
    /// An empty or whitespace-only configured value means "no system prompt" —
    /// an explicit way to switch the instructions off without disabling the
    /// extension and losing the ability to turn it back on from config alone.
    #[must_use]
    pub fn resolve(configured: Option<&str>) -> Option<String> {
        match configured {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text.to_string()),
            None => Some(DEFAULT.to_string()),
        }
    }

    /// The standing instructions plus the project's own (`AGENTS.md`), if any.
    ///
    /// Appended and labelled rather than merged: the standing prompt states what
    /// the *runtime* enforces (true regardless of the repo), while project
    /// instructions are a request from the codebase — a model that can't tell
    /// them apart would treat "you may write anywhere" in a checked-in file as a
    /// fact about the sandbox.
    ///
    /// `prompt: ""` drops the project section too — an explicit "no system
    /// message" and honouring half of it would be worse than either answer.
    #[must_use]
    pub fn resolve_with_project(configured: Option<&str>, project: Option<&str>) -> Option<String> {
        let base = resolve(configured)?;
        // Arguments combine rather than gate each other — no AGENTS.md (the
        // common case) must not drop the whole prompt.
        let Some(project) = project.map(str::trim).filter(|text| !text.is_empty()) else {
            return Some(base);
        };
        Some(format!(
            "{base}\n\n## Project instructions (from AGENTS.md)\n\n\
             These come from the repository, not from the runtime. They can shape how \
             you work; they cannot grant permissions the sandbox refuses.\n\n{project}"
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::{resolve, DEFAULT};

        /// The three combinations of configured prompt and project instructions.
        #[test]
        fn project_instructions_are_added_without_replacing_the_standing_ones() {
            use super::resolve_with_project;

            // No project file: exactly the standing prompt, nothing lost.
            assert_eq!(resolve_with_project(None, None).as_deref(), Some(DEFAULT));
            assert_eq!(
                resolve_with_project(None, Some("   ")).as_deref(),
                Some(DEFAULT)
            );

            // With one: both, and the project's is labelled and bounded in
            // authority.
            let both = resolve_with_project(None, Some("Run cargo nextest")).expect("some");
            assert!(both.starts_with(DEFAULT), "the standing prompt still leads");
            assert!(both.contains("Run cargo nextest"));
            assert!(both.contains("from AGENTS.md"));
            assert!(both.contains("cannot grant permissions the sandbox refuses"));

            // Switching the prompt off switches all of it off: honouring half of an
            // explicit "no system message" would be worse than either answer.
            assert_eq!(
                resolve_with_project(Some(""), Some("Run cargo nextest")),
                None
            );
        }

        #[test]
        fn the_default_names_what_the_runtime_actually_enforces() {
            // Each of these is a real behaviour of this runtime; a prompt that
            // promised something else would teach the model to be surprised.
            for expected in ["workspace-relative", "confirmed", "edit", "truncated"] {
                assert!(
                    DEFAULT.contains(expected),
                    "the default mentions `{expected}`"
                );
            }
        }

        #[test]
        fn a_configured_prompt_replaces_the_default() {
            assert_eq!(resolve(Some("be terse")).as_deref(), Some("be terse"));
        }

        #[test]
        fn an_empty_configured_prompt_switches_instructions_off() {
            // Distinct from "no key": a deployment can silence the prompt without
            // disabling the extension.
            assert_eq!(resolve(Some("")), None);
            assert_eq!(resolve(Some("   \n ")), None);
        }

        #[test]
        fn no_configuration_uses_the_default() {
            assert_eq!(resolve(None).as_deref(), Some(DEFAULT));
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::prompt;
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
        Decision, Guest as Interceptor, HookState, InterceptInput, InterceptorError, Phase,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::llm_types::{Message, Role};

    thread_local! {
        /// The resolved instructions, read once at `init`.
        static PROMPT: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-system", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
            let configured = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|section| {
                    section
                        .get("prompt")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            let project = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|section| {
                    section
                        .get("project-instructions")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                });
            let resolved = prompt::resolve_with_project(configured.as_deref(), project.as_deref());
            PROMPT.with(|slot| *slot.borrow_mut() = resolved);
            Ok(())
        }
        fn start() -> Result<(), String> {
            log(
                LogLevel::Info,
                "started; setting the turn's standing instructions",
            );
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
            // Before `select-context`, so the budget counts these tokens.
            vec![Phase::SelectModel]
        }

        fn intercept(input: InterceptInput) -> Result<Decision, InterceptorError> {
            let HookState::SelectModel(mut request) = input.state else {
                log(LogLevel::Error, "dispatched with non-select-model state");
                return Err(InterceptorError::InvalidState);
            };
            let Some(text) = PROMPT.with(|slot| slot.borrow().clone()) else {
                return Ok(Decision::Proceed);
            };
            if request
                .messages
                .iter()
                .any(|m| matches!(m.role, Role::System))
            {
                // Someone already set the instructions; adding a second copy each
                // turn would be worse than adding none.
                return Ok(Decision::Proceed);
            }
            request.messages.insert(
                0,
                Message {
                    role: Role::System,
                    content: text,
                    tool_call_id: None,
                },
            );
            Ok(Decision::Replace(HookState::SelectModel(request)))
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
