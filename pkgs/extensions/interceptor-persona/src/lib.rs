//! `interceptor-persona`: personality as data, read from configuration.
//! Injects the configured default persona at `select-model`, before `interceptor-context`
//! measures the token budget. An unknown persona falls back to the built-in text with a warning.

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
        /// The persona text in force: the configured default, or the built-in one.
        pub default: String,
    }

    impl PersonaConfig {
        /// Parse from the extension's config section. Returns `None` when disabled or invalid,
        /// plus an optional warning.
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

            let enabled = config_obj
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            if !enabled {
                return (None, None);
            }

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

            let default_name = config_obj
                .get("default")
                .and_then(|v| v.as_str())
                .unwrap_or("helpful")
                .to_string();

            let default_text = personalities
                .get(&default_name)
                .cloned()
                // Unknown or absent name: the built-in text serves.
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
                return Ok(Decision::Proceed);
            };

            // Idempotent: skip when a persona message is already present.
            if request
                .messages
                .iter()
                .any(|m| m.content.starts_with("## Persona\n"))
            {
                return Ok(Decision::Proceed);
            }

            // User role, not system: keeps it distinct from standing instructions.
            let persona_msg = Message {
                role: Role::User,
                content: format!("## Persona\n{}", config.default),
                tool_call_id: None,
            };

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
            Contributions {
                commands: vec![],
                status_items: vec![],
                forms: vec![],
            }
        }

        fn invoke(_name: String, _arguments: Vec<ArgumentValue>) -> Result<Outcome, InvokeError> {
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
