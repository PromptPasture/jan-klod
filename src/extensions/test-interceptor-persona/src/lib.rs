//! Test guest for `interceptor-persona` — verifies persona configuration loading
//! and injection into model requests at select-model phase.
//!
//! Tests:
//! 1. With `enabled: true` and `default: expert`, persona text is injected.
//! 2. With `enabled: false`, no persona text is injected.
//! 3. With invalid persona name, host logs warning and defaults to "helpful".

// Pure logic: unit-tested natively; WASM glue compiles for wasm32 only.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod persona_logic {
    use serde_json::Value;

    /// Test configuration for persona loading.
    #[derive(Clone, Debug)]
    pub struct TestPersonaConfig {
        pub enabled: bool,
        pub default: String,
    }

    impl TestPersonaConfig {
        /// Parse test config from JSON to verify loading behavior.
        /// Returns the config if enabled=true, None if enabled=false,
        /// and logs a warning if the default persona is not found.
        pub fn from_config(raw_json: &str) -> (Option<Self>, Option<String>) {
            let config_value: Value = match serde_json::from_str(raw_json) {
                Ok(v) => v,
                Err(_) => return (None, Some("Failed to parse config as JSON".to_string())),
            };

            let Some(config_obj) = config_value.as_object() else {
                return (None, Some("Config is not an object".to_string()));
            };

            // Check if enabled (default false)
            let enabled = config_obj
                .get("enabled")
                .and_then(|v| v.as_bool())
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
                .unwrap_or_else(|| {
                    "You are helpful, concise, and focused on solving the user's problem."
                        .to_string()
                });

            let warning = if !personalities.contains_key(&default_name) && !personalities.is_empty()
            {
                Some(format!(
                    "Persona '{}' not found in personalities; using built-in default",
                    default_name
                ))
            } else {
                None
            };

            (
                Some(TestPersonaConfig {
                    enabled,
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
        fn test_enabled_with_expert_default() {
            let json = r#"{
                "enabled": true,
                "default": "expert",
                "personalities": {
                    "helpful": "You are helpful and concise.",
                    "expert": "You are a technical expert."
                }
            }"#;
            let (config, warn) = TestPersonaConfig::from_config(json);
            assert!(config.is_some(), "Config should be Some when enabled=true");
            assert!(
                warn.is_none(),
                "No warning should be issued for valid expert persona"
            );
            let cfg = config.unwrap();
            assert!(cfg.enabled, "enabled should be true");
            assert_eq!(
                cfg.default, "You are a technical expert.",
                "expert persona should be loaded"
            );
        }

        #[test]
        fn test_disabled_returns_none() {
            let json = r#"{"enabled": false}"#;
            let (config, warn) = TestPersonaConfig::from_config(json);
            assert!(config.is_none(), "Config should be None when enabled=false");
            assert!(warn.is_none(), "No warning should be issued when disabled");
        }

        #[test]
        fn test_invalid_persona_name_logs_warning() {
            let json = r#"{
                "enabled": true,
                "default": "nonexistent",
                "personalities": {
                    "helpful": "You are helpful.",
                    "expert": "You are expert."
                }
            }"#;
            let (config, warn) = TestPersonaConfig::from_config(json);
            assert!(
                config.is_some(),
                "Config should still be Some with invalid persona"
            );
            assert!(
                warn.is_some(),
                "Warning should be issued for nonexistent persona"
            );
            let warning_msg = warn.unwrap();
            assert!(
                warning_msg.contains("nonexistent"),
                "Warning should mention the invalid persona name"
            );
            // Should default to helpful built-in
            let cfg = config.unwrap();
            assert!(
                cfg.default.contains("helpful"),
                "Should default to helpful text when persona not found"
            );
        }

        #[test]
        fn test_missing_enabled_defaults_to_false() {
            let json = r#"{"default": "helpful"}"#;
            let (config, _) = TestPersonaConfig::from_config(json);
            assert!(
                config.is_none(),
                "Config should be None when enabled is missing (defaults to false)"
            );
        }

        #[test]
        fn test_no_personalities_uses_builtin_default() {
            let json = r#"{"enabled": true}"#;
            let (config, warn) = TestPersonaConfig::from_config(json);
            assert!(config.is_some(), "Config should be Some when enabled=true");
            assert!(warn.is_none(), "No warning when no custom personas exist");
            let cfg = config.unwrap();
            assert!(
                cfg.default.contains("helpful"),
                "Should use built-in helpful default"
            );
        }

        #[test]
        fn test_explicit_helpful_persona() {
            let json = r#"{
                "enabled": true,
                "default": "helpful",
                "personalities": {
                    "helpful": "You are a helpful assistant."
                }
            }"#;
            let (config, warn) = TestPersonaConfig::from_config(json);
            assert!(config.is_some());
            assert!(warn.is_none());
            let cfg = config.unwrap();
            assert_eq!(
                cfg.default, "You are a helpful assistant.",
                "Should use explicitly configured helpful persona"
            );
        }
    }
}

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
            world: "interceptor-contributor-world",
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

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            // Load and parse the config to verify it was set correctly
            let config_str = host_config::all().unwrap_or_else(|_| "{}".to_owned());

            // Log what we received for debugging
            host_log::log(
                LogLevel::Info,
                "test-interceptor-persona",
                &format!("Config received: {}", config_str),
                &[],
            );

            Ok(())
        }

        fn start() -> Result<(), String> {
            host_log::log(
                LogLevel::Info,
                "test-interceptor-persona",
                "test interceptor persona started",
                &[],
            );
            Ok(())
        }

        fn stop() {
            host_log::log(
                LogLevel::Info,
                "test-interceptor-persona",
                "test interceptor persona stopping",
                &[],
            );
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
            let HookState::SelectModel(request) = input.state else {
                return Err(InterceptorError::InvalidState);
            };

            // Log what messages are in the request for test verification
            let msg_summary = request
                .messages
                .iter()
                .map(|m| &m.content[..std::cmp::min(50, m.content.len())])
                .collect::<Vec<_>>()
                .join(" | ");

            host_log::log(
                LogLevel::Info,
                "test-interceptor-persona",
                &format!("Request has messages: {}", msg_summary),
                &[],
            );

            Ok(Decision::Proceed)
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

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!("This is a WebAssembly component, not a native executable.");
}
