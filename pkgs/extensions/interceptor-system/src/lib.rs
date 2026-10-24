//! `interceptor-system`: standing instructions inserted at `select-model` only when no system message exists.

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod prompt {
    /// Each line names something the runtime enforces, so the model's expectations match the sandbox.
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

    /// An empty or whitespace-only value switches instructions off.
    #[must_use]
    pub fn resolve(configured: Option<&str>) -> Option<String> {
        match configured {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text.to_string()),
            None => Some(DEFAULT.to_string()),
        }
    }

    /// `prompt: ""` is deliberate, so it is not reported as unconfigured.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Source {
        BuiltIn,
        Configured,
        Off,
    }

    impl Source {
        #[must_use]
        pub const fn label(self) -> &'static str {
            match self {
                Self::BuiltIn => "built-in",
                Self::Configured => "configured",
                Self::Off => "off",
            }
        }

        #[must_use]
        pub const fn detail(self) -> &'static str {
            match self {
                Self::BuiltIn => "no `prompt` is configured, so the built-in instructions run",
                Self::Configured => "a configured `prompt` replaces the built-in instructions",
                Self::Off => "`prompt` is empty, so no system message is sent",
            }
        }
    }

    #[must_use]
    pub fn source(configured: Option<&str>) -> Source {
        match configured {
            None => Source::BuiltIn,
            Some(text) if text.trim().is_empty() => Source::Off,
            Some(_) => Source::Configured,
        }
    }

    /// A status area cannot show a long text.
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

    /// Labelled as a repo request: a checked-in file's claims are not sandbox facts.
    #[must_use]
    pub fn resolve_with_project(configured: Option<&str>, project: Option<&str>) -> Option<String> {
        let base = resolve(configured)?;
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

        #[test]
        fn project_instructions_are_added_without_replacing_the_standing_ones() {
            use super::resolve_with_project;

            assert_eq!(resolve_with_project(None, None).as_deref(), Some(DEFAULT));
            assert_eq!(
                resolve_with_project(None, Some("   ")).as_deref(),
                Some(DEFAULT)
            );

            let both = resolve_with_project(None, Some("Run cargo nextest")).expect("some");
            assert!(both.starts_with(DEFAULT), "the standing prompt still leads");
            assert!(both.contains("Run cargo nextest"));
            assert!(both.contains("from AGENTS.md"));
            assert!(both.contains("cannot grant permissions the sandbox refuses"));

            assert_eq!(
                resolve_with_project(Some(""), Some("Run cargo nextest")),
                None
            );
        }

        #[test]
        fn the_default_names_what_the_runtime_actually_enforces() {
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
        static PROMPT: RefCell<Option<String>> = const { RefCell::new(None) };
        /// A configured prompt equal to the built-in one is still configured.
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

    const PROMPT_COMMAND: &str = "prompt";
    const VERBOSE: &str = "verbose";

    impl ClientSurface for Component {
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
            let verbose = arguments.iter().any(|argument| {
                argument.name == VERBOSE && matches!(argument.value.trim(), "true" | "yes" | "1")
            });
            let text = PROMPT.with(|slot| prompt::summary(slot.borrow().as_deref(), verbose));
            Ok(Outcome {
                text,
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
