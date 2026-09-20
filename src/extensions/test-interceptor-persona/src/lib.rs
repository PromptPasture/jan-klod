//! Test guest for `interceptor-persona` — verifies persona configuration loading
//! and injection into model requests at select-model phase.
//!
//! Tests persona behavior at the WIT boundary:
//! 1. With `enabled: true`, persona text is injected into request messages.
//! 2. With `enabled: false`, no persona text is injected.
//! 3. Invalid persona names trigger warnings and fallback to built-in.

// Pure logic: unit-tested natively; WASM glue compiles for wasm32 only.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod persona_verification {
    use serde_json::Value;
    use std::collections::HashMap;

    /// Check if interceptor-persona is configured as enabled.
    pub fn is_persona_enabled(config_json: &str) -> bool {
        if let Ok(config_value) = serde_json::from_str::<Value>(config_json) {
            if let Some(config_obj) = config_value.as_object() {
                if let Some(extensions) = config_obj.get("extensions") {
                    if let Some(extensions_obj) = extensions.as_object() {
                        if let Some(interceptor) = extensions_obj.get("interceptor") {
                            if let Some(interceptor_obj) = interceptor.as_object() {
                                if let Some(persona) = interceptor_obj.get("persona") {
                                    if let Some(persona_obj) = persona.as_object() {
                                        return persona_obj
                                            .get("enabled")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Extract personas map and default name from config.
    /// Returns (personalities_map, default_name).
    pub fn extract_personas(config_json: &str) -> (HashMap<String, String>, String) {
        let mut personalities = HashMap::new();
        let mut default_name = "helpful".to_string();

        if let Ok(config_value) = serde_json::from_str::<Value>(config_json) {
            if let Some(config_obj) = config_value.as_object() {
                if let Some(extensions) = config_obj.get("extensions") {
                    if let Some(extensions_obj) = extensions.as_object() {
                        if let Some(interceptor) = extensions_obj.get("interceptor") {
                            if let Some(interceptor_obj) = interceptor.as_object() {
                                if let Some(persona) = interceptor_obj.get("persona") {
                                    if let Some(persona_obj) = persona.as_object() {
                                        // Extract personalities map
                                        if let Some(personas_value) =
                                            persona_obj.get("personalities")
                                        {
                                            if let Some(personas_obj) = personas_value.as_object() {
                                                for (name, desc) in personas_obj {
                                                    if let Some(description) = desc.as_str() {
                                                        personalities.insert(
                                                            name.clone(),
                                                            description.to_string(),
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        // Extract default name
                                        if let Some(default_value) = persona_obj.get("default") {
                                            if let Some(name) = default_value.as_str() {
                                                default_name = name.to_string();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        (personalities, default_name)
    }

    /// Verify persona injection: assert the configured persona text is present when enabled,
    /// and absent when disabled. Also verifies that only the configured persona is injected.
    /// Returns Ok(()) if assertion passes, Err with message if assertion fails.
    pub fn verify_injection(config_json: &str, messages: &[String]) -> Result<(), String> {
        let enabled = is_persona_enabled(config_json);
        let (personalities, default_name) = extract_personas(config_json);

        // Combine all message contents
        let all_message_content = messages.join("\n");
        let has_persona_section = all_message_content.contains("## Persona");

        if enabled && !has_persona_section {
            return Err(
                "ASSERTION FAILED: persona enabled in config but no '## Persona' section found in request"
                    .to_string(),
            );
        }

        if !enabled && has_persona_section {
            return Err(
                "ASSERTION FAILED: persona disabled in config but '## Persona' section was injected"
                    .to_string(),
            );
        }

        // When enabled, verify the configured persona text is present
        if enabled {
            // Get the text of the configured default persona
            let configured_text = personalities
                .get(&default_name)
                .cloned()
                .unwrap_or_else(|| {
                    // If no custom personas or default not found, should be the built-in default
                    "You are helpful, concise, and focused on solving the user's problem."
                        .to_string()
                });

            if !all_message_content.contains(&configured_text) {
                return Err(format!(
                    "ASSERTION FAILED: persona '{}' text not found in request. Expected: '{}'",
                    default_name, configured_text
                ));
            }

            // Verify that other personas' text is NOT present
            for (persona_name, persona_text) in &personalities {
                if persona_name != &default_name && all_message_content.contains(persona_text) {
                    return Err(format!(
                        "ASSERTION FAILED: non-configured persona '{}' text found in request. Only '{}' should be injected.",
                        persona_name, default_name
                    ));
                }
            }
        }

        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_persona_enabled_detection() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "expert",
                            "personalities": {
                                "expert": "You are a technical expert."
                            }
                        }
                    }
                }
            }"#;
            assert!(is_persona_enabled(config));
        }

        #[test]
        fn test_persona_disabled_detection() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": false
                        }
                    }
                }
            }"#;
            assert!(!is_persona_enabled(config));
        }

        #[test]
        fn test_verification_enabled_with_configured_persona_present() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "expert",
                            "personalities": {
                                "expert": "You are a technical expert.",
                                "helpful": "You are helpful and concise."
                            }
                        }
                    }
                }
            }"#;
            let messages = vec!["## Persona\nYou are a technical expert.".to_string()];
            assert!(verify_injection(config, &messages).is_ok());
        }

        #[test]
        fn test_verification_enabled_without_persona_fails() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "expert",
                            "personalities": {
                                "expert": "You are a technical expert."
                            }
                        }
                    }
                }
            }"#;
            let messages = vec!["Some other message".to_string()];
            assert!(verify_injection(config, &messages).is_err());
        }

        #[test]
        fn test_verification_enabled_with_wrong_persona_text_fails() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "expert",
                            "personalities": {
                                "expert": "You are a technical expert.",
                                "helpful": "You are helpful and concise."
                            }
                        }
                    }
                }
            }"#;
            // Message has a persona section but with the wrong persona's text
            let messages = vec!["## Persona\nYou are helpful and concise.".to_string()];
            assert!(verify_injection(config, &messages).is_err());
        }

        #[test]
        fn test_verification_enabled_with_non_configured_persona_present_fails() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "expert",
                            "personalities": {
                                "expert": "You are a technical expert.",
                                "helpful": "You are helpful and concise."
                            }
                        }
                    }
                }
            }"#;
            // Message has the correct persona text AND another persona's text
            let messages = vec![
                "## Persona\nYou are a technical expert.".to_string(),
                "Additional context: You are helpful and concise.".to_string(),
            ];
            assert!(verify_injection(config, &messages).is_err());
        }

        #[test]
        fn test_verification_disabled_without_persona_passes() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": false
                        }
                    }
                }
            }"#;
            let messages = vec!["Some other message".to_string()];
            assert!(verify_injection(config, &messages).is_ok());
        }

        #[test]
        fn test_verification_disabled_with_persona_fails() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": false
                        }
                    }
                }
            }"#;
            let messages = vec!["## Persona\nYou are helpful.".to_string()];
            assert!(verify_injection(config, &messages).is_err());
        }

        #[test]
        fn test_verification_invalid_default_name_falls_back_to_builtin() {
            let config = r#"{
                "extensions": {
                    "interceptor": {
                        "persona": {
                            "enabled": true,
                            "default": "nonexistent",
                            "personalities": {
                                "helpful": "You are helpful and concise."
                            }
                        }
                    }
                }
            }"#;
            // When default persona name doesn't exist, should fall back to built-in
            let builtin_text =
                "You are helpful, concise, and focused on solving the user's problem.";
            let messages = vec![format!("## Persona\n{}", builtin_text)];
            assert!(verify_injection(config, &messages).is_ok());
        }

        #[test]
        fn test_missing_persona_section_treated_as_disabled() {
            let config = r#"{"extensions": {"interceptor": {}}}"#;
            assert!(!is_persona_enabled(config));
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

            // Get the full config to verify persona injection behavior
            let config_str = host_config::all().unwrap_or_else(|_| "{}".to_owned());

            // Extract message contents for verification
            let message_contents: Vec<String> =
                request.messages.iter().map(|m| m.content.clone()).collect();

            // Verify persona injection: persona text must be present if enabled, absent if disabled
            match persona_verification::verify_injection(&config_str, &message_contents) {
                Ok(()) => {
                    // Assertion passed; log for debugging and proceed
                    let persona_status = if persona_verification::is_persona_enabled(&config_str) {
                        "present (enabled)"
                    } else {
                        "absent (disabled)"
                    };
                    host_log::log(
                        LogLevel::Info,
                        "test-interceptor-persona",
                        &format!("Persona injection verified: {}", persona_status),
                        &[],
                    );
                    Ok(Decision::Proceed)
                }
                Err(assertion_error) => {
                    // Assertion failed; log error and reject
                    host_log::log(
                        LogLevel::Error,
                        "test-interceptor-persona",
                        &assertion_error,
                        &[],
                    );
                    Err(InterceptorError::ValidationFailed)
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

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    println!("This is a WebAssembly component, not a native executable.");
}
