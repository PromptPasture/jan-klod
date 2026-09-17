//! `interceptor-system` — the standing instructions a turn runs under.
//! Without it, the model gets raw conversation with no framing: it isn't told
//! it's an agent, that paths are workspace-relative, or that writes get confirmed.
//! For small models, that framing is critical.
//!
//! ## Why an extension, and why `select-model`
//!
//! A system prompt is policy, so it lives in a swappable guest rather than core.
//! It runs at `select-model` (not `select-context`) because that phase runs first:
//! the prompt must be in the message list when `interceptor-context` measures
//! the token budget, or those tokens go uncounted.
//!
//! ## Idempotence
//!
//! Prepends only when no system message exists, so a turn that already has one
//! (driver-set, another interceptor, replayed) doesn't accumulate copies.
//!
//! The assembly is pure Rust (unit-tested natively); the glue compiles for `wasm32`.

// Pure logic: unit-tested natively; CM glue compiles for wasm32 only.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod prompt {
    /// The built-in instructions, used when config supplies no `prompt`.
    ///
    /// Each line names something the runtime actually enforces, so the model's
    /// expectations match the sandbox's behaviour: paths are jailed, writes are
    /// confirmed, partial edits are anchored, and output goes into the budget.
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
    /// An empty or whitespace-only value means "no system prompt" — an explicit
    /// way to switch instructions off without disabling the extension.
    #[must_use]
    pub fn resolve(configured: Option<&str>) -> Option<String> {
        match configured {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text.to_string()),
            None => Some(DEFAULT.to_string()),
        }
    }

    /// Where this turn's instructions came from — the answer the `prompt-source`
    /// status item shows.
    ///
    /// Three states rather than a bool: "off" is a deliberate configuration
    /// (`prompt: ""`), and showing it as "not configured" would read as an
    /// oversight rather than a choice.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Source {
        /// The built-in text, because nothing was configured.
        BuiltIn,
        /// An operator wrote a `prompt`.
        Configured,
        /// `prompt: ""` — no system message at all.
        Off,
    }

    impl Source {
        /// Short label for a status line.
        #[must_use]
        pub const fn label(self) -> &'static str {
            match self {
                Self::BuiltIn => "built-in",
                Self::Configured => "configured",
                Self::Off => "off",
            }
        }

        /// One line of why, for a client with room for it.
        #[must_use]
        pub const fn detail(self) -> &'static str {
            match self {
                Self::BuiltIn => "no `prompt` is configured, so the built-in instructions run",
                Self::Configured => "a configured `prompt` replaces the built-in instructions",
                Self::Off => "`prompt` is empty, so no system message is sent",
            }
        }
    }

    /// Classify what the configured value means.
    #[must_use]
    pub fn source(configured: Option<&str>) -> Source {
        match configured {
            None => Source::BuiltIn,
            Some(text) if text.trim().is_empty() => Source::Off,
            Some(_) => Source::Configured,
        }
    }

    /// What the `prompt` command answers with.
    ///
    /// `verbose` gives the whole text; otherwise the first line and a length,
    /// because a client may be rendering this into a single status area and a
    /// thousand-character answer there is not a help.
    #[must_use]
    pub fn summary(resolved: Option<&str>, verbose: bool) -> String {
        let Some(text) = resolved else {
            return "No system message is sent: `prompt` is empty.".to_owned();
        };
        if verbose {
            return text.to_owned();
        }
        let first = text.lines().next().unwrap_or_default();
        let characters = text.chars().count();
        format!("{first} … ({characters} characters; pass `verbose` for all of it)")
    }

    /// The standing instructions plus the project's own (`AGENTS.md`), if any.
    ///
    /// Appended and labelled rather than merged: the standing prompt states what
    /// the *runtime* enforces (true regardless of repo), while project instructions
    /// are a request from the codebase — a model that can't distinguish them would
    /// treat "you may write anywhere" in a checked-in file as a sandbox fact.
    ///
    /// `prompt: ""` drops the project section too — an explicit "no system message".
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
        use super::{resolve, source, summary, Source, DEFAULT};

        #[test]
        fn an_unconfigured_prompt_reads_as_built_in() {
            assert_eq!(source(None), Source::BuiltIn);
        }

        #[test]
        fn a_configured_prompt_says_so() {
            assert_eq!(source(Some("be terse")), Source::Configured);
        }

        /// `prompt: ""` is a choice, and the status line has to say that rather
        /// than report it as an absent configuration.
        #[test]
        fn an_empty_prompt_reads_as_off_not_as_unconfigured() {
            assert_eq!(source(Some("   ")), Source::Off);
            assert_ne!(source(Some("")).label(), source(None).label());
        }

        #[test]
        fn a_summary_leads_with_the_first_line_and_says_how_much_is_left() {
            let text = "First line.\nSecond line.";
            let short = summary(Some(text), false);
            assert!(short.starts_with("First line."), "{short}");
            assert!(short.contains("24 characters"), "{short}");
            assert!(!short.contains("Second line."), "{short}");
        }

        #[test]
        fn a_verbose_summary_is_the_text_itself() {
            assert_eq!(summary(Some(DEFAULT), true), DEFAULT);
        }

        #[test]
        fn a_summary_of_no_prompt_says_none_is_sent() {
            let none = summary(None, true);
            assert!(none.contains("No system message"), "{none}");
        }

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
            world: "interceptor-contributor-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::client_surface::{
        Argument, ArgumentValue, Command, Contributions, Guest as ClientSurface, InvokeError,
        Outcome, StatusItem,
    };
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
        /// Where they came from, for the `prompt-source` status item. Kept
        /// beside the text because the text alone cannot say it: a configured
        /// prompt that happens to equal the built-in one is still configured.
        static SOURCE: RefCell<prompt::Source> = const { RefCell::new(prompt::Source::BuiltIn) };
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
            SOURCE.with(|slot| *slot.borrow_mut() = prompt::source(configured.as_deref()));
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

    /// The name a client invokes to see the standing instructions.
    const PROMPT_COMMAND: &str = "prompt";
    /// The argument that asks for the whole text rather than a summary.
    const VERBOSE: &str = "verbose";

    impl ClientSurface for Component {
        /// What this extension offers a client to render.
        ///
        /// Two things, both about the instructions it already owns: a command
        /// to read them and a status item saying where they came from. It
        /// declares no form — there is nothing here to fill in, and a form
        /// nobody needs is a shape three renderers would have to get right.
        fn contribute() -> Contributions {
            let source = SOURCE.with(|slot| *slot.borrow());
            Contributions {
                commands: vec![Command {
                    name: PROMPT_COMMAND.to_owned(),
                    title: "System prompt".to_owned(),
                    description: "Show the standing instructions this turn runs under".to_owned(),
                    arguments: vec![Argument {
                        name: VERBOSE.to_owned(),
                        description: "Show the whole text rather than its first line".to_owned(),
                        required: false,
                    }],
                }],
                status_items: vec![StatusItem {
                    name: "prompt-source".to_owned(),
                    text: source.label().to_owned(),
                    detail: source.detail().to_owned(),
                }],
                forms: vec![],
            }
        }

        fn invoke(name: String, arguments: Vec<ArgumentValue>) -> Result<Outcome, InvokeError> {
            if name != PROMPT_COMMAND {
                return Err(InvokeError::Unknown);
            }
            // Absent means no, and so does any value that is not a plain yes —
            // a client that sends `verbose: maybe` gets the summary rather
            // than an error, because the argument is a convenience, not a gate.
            let verbose = arguments.iter().any(|argument| {
                argument.name == VERBOSE && matches!(argument.value.trim(), "true" | "yes" | "1")
            });
            let text = PROMPT.with(|slot| prompt::summary(slot.borrow().as_deref(), verbose));
            Ok(Outcome {
                text,
                // Reading the prompt does not change what is contributed; the
                // status item only moves when config does, which is a reboot.
                contributions_changed: false,
            })
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
