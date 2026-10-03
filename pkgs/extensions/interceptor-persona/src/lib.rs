//! `interceptor-persona` — personality as data, read from configuration.
//!
//! Personas are opaque text descriptions of preferred behavior. This extension
//! reads a configured default persona at init and injects it into each turn's
//! context before model selection.
//!
//! ## Why an extension, and why `select-model`
//!
//! Persona is policy — it lives in a swappable guest rather than core.
//! It runs at `select-model` (not `select-context`) because that phase runs first:
//! the persona must be in the message list when `interceptor-context` measures
//! the token budget, or those tokens go uncounted.
//!
//! ## Scope (this slice)
//!
//! Reads the configured `default` persona; per-principal selection comes later
//! when Phase 28c lands. If no valid persona is found, defaults to a helpful
//! built-in text and logs a warning.

// Pure logic: unit-tested natively; CM glue compiles for wasm32 only.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod persona {
    use serde_json::Value;

    /// The built-in helpful persona, used when config supplies no personas.
    pub const DEFAULT_HELPFUL: &str =
        "You are helpful, concise, and focused on solving the user's problem.";

    /// Persona configuration deserialized from extension config.
    #[derive(Clone, Debug)]
    pub struct PersonaConfig {
        /// The persona text in force: the configured default, or the built-in
        /// one. The other personas are resolved at parse time and not kept —
        /// nothing reads them afterwards.
        pub default: String,
    }

    impl PersonaConfig {
        /// Parse `PersonaConfig` from the extension's config section (JSON).
        /// Returns None if no valid config, along with an optional warning message.
        pub fn from_config(raw_json: &str) -> (Option<Self>, Option<String>) {
            let config_value: Value = match serde_json::from_str(raw_json) {
                Ok(v) => v,
                Err(_) => {
                    return (
                        None,
                        Some("Failed to parse persona config as JSON".to_string()),
                    )
                }
            };

            let Some(config_obj) = config_value.as_object() else {
                return (None, Some("Persona config is not an object".to_string()));
            };

            // Check if enabled (default false)
            let enabled = config_obj
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            if !enabled {
                return (None, None);
            }

            // Parse personalities map
            let mut personalities = std::collections::HashMap::new();
            if let Some(personas_value) = config_obj.get("personalities") {
                if let Some(personas_obj) = personas_value.as_object() {
                    for (name, desc) in personas_obj {
                        if let Some(description) = desc.as_str() {
                            personalities.insert(name.clone(), description.to_string());
                        }
                    }
                }
            }

            // Get default persona name (default to "helpful" if not specified)
            let default_name = config_obj
                .get("default")
                .and_then(|v| v.as_str())
                .unwrap_or("helpful")
                .to_string();

            // Resolve the default persona text
            let default_text = personalities
                .get(&default_name)
                .cloned()
                // No custom personas, or the named default is not among them:
                // the built-in text serves (the warning below says which case).
                .unwrap_or_else(|| DEFAULT_HELPFUL.to_string());

            let warning = if !personalities.contains_key(&default_name) && !personalities.is_empty()
            {
                Some(format!(
                    "Persona '{default_name}' not found in personalities; using built-in default"
                ))
            } else {
                None
            };

            (
                Some(Self {
                    default: default_text,
                }),
                warning,
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_enabled_with_personalities() {
            let json = r#"{
                "enabled": true,
                "personalities": {
                    "helpful": "You are helpful and concise.",
                    "expert": "You are a technical expert."
                },
                "default": "expert"
            }"#;
            let (config, warn) = PersonaConfig::from_config(json);
            assert!(config.is_some());
            assert!(warn.is_none());
            let cfg = config.unwrap();
            assert_eq!(cfg.default, "You are a technical expert.");
        }

        #[test]
        fn parse_disabled() {
            let json = r#"{"enabled": false}"#;
            let (config, _) = PersonaConfig::from_config(json);
            assert!(config.is_none());
        }

        #[test]
        fn parse_missing_enabled() {
            let json = r#"{"personalities": {"helpful": "text"}}"#;
            let (config, _) = PersonaConfig::from_config(json);
            assert!(config.is_none());
        }

        #[test]
        fn parse_invalid_default() {
            let json = r#"{
                "enabled": true,
                "personalities": {"helpful": "You are helpful."},
                "default": "nonexistent"
            }"#;
            let (config, warn) = PersonaConfig::from_config(json);
            assert!(config.is_some());
            assert!(warn.is_some());
        }

        #[test]
        fn parse_no_custom_personas() {
            let json = r#"{"enabled": true}"#;
            let (config, _) = PersonaConfig::from_config(json);
            assert!(config.is_some());
            let cfg = config.unwrap();
            assert_eq!(cfg.default, DEFAULT_HELPFUL);
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::persona;
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
        ArgumentValue, Contributions, Guest as ClientSurface, InvokeError, Outcome,
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
        /// The resolved persona configuration.
        static PERSONA_CONFIG: RefCell<Option<persona::PersonaConfig>> = const { RefCell::new(None) };
    }

    fn log(level: LogLevel, message: &str) {
        host_log::log(level, "interceptor-persona", message, &[]);
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());

            // Extract the persona section from the full config
            let persona_json = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|config_value| {
                    config_value
                        .get("extensions")?
                        .get("interceptor")?
                        .get("persona")
                        .cloned()
                })
                .and_then(|persona| serde_json::to_string(&persona).ok())
                .unwrap_or_else(|| "{}".to_owned());

            let (config, warning) = persona::PersonaConfig::from_config(&persona_json);

            if let Some(warn_msg) = warning {
                log(LogLevel::Warn, &warn_msg);
            }

            if config.is_some() {
                log(LogLevel::Info, "initialized with persona configuration");
            } else {
                log(
                    LogLevel::Info,
                    "persona disabled or not configured; extension inactive",
                );
            }

            PERSONA_CONFIG.with(|slot| *slot.borrow_mut() = config);

            Ok(())
        }

        fn start() -> Result<(), String> {
            log(LogLevel::Info, "started");
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

            let Some(config) = PERSONA_CONFIG.with(|slot| slot.borrow().clone()) else {
                // Persona not configured; pass through
                return Ok(Decision::Proceed);
            };

            // Check if a persona message already exists (from driver or previous interceptor)
            if request
                .messages
                .iter()
                .any(|m| m.content.starts_with("## Persona\n"))
            {
                // Simple check: avoid duplicating persona instructions
                return Ok(Decision::Proceed);
            }

            // Insert persona as a user-facing context message, after system but before other content.
            // Use a distinct role to distinguish from standing instructions.
            let persona_msg = Message {
                role: Role::User,
                content: format!("## Persona\n{}", config.default),
                tool_call_id: None,
            };

            // Find the position after the system message, if present
            let insert_pos = usize::from(
                request
                    .messages
                    .iter()
                    .any(|m| matches!(m.role, Role::System)),
            );

            request.messages.insert(insert_pos, persona_msg);
            Ok(Decision::Replace(HookState::SelectModel(request)))
        }
    }

    impl ClientSurface for Component {
        fn contribute() -> Contributions {
            // This extension does not expose any commands or status items
            Contributions {
                commands: vec![],
                status_items: vec![],
                forms: vec![],
            }
        }

        fn invoke(_name: String, _arguments: Vec<ArgumentValue>) -> Result<Outcome, InvokeError> {
            // No commands to invoke
            Err(InvokeError::Unknown)
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
