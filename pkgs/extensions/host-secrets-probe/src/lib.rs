//! Guest that probes `host-secrets`: denied, not-found, found, and refused-at-load paths.

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
            world: "tool-world",
            path: "../../../wit",
        });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_log::{self, LogLevel};
    use bindings::jan_klod::interfaces::host_secrets;

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            host_log::log(
                LogLevel::Info,
                "host-secrets-probe",
                &format!("init id={}", ctx.id),
                &[],
            );
            Ok(())
        }
        fn start() -> Result<(), String> {
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "secrets-probe".to_string(),
                description: "Test instrument: attempts to read secrets via host-secrets."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Secret name to retrieve" }
                    },
                    "required": ["name"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;

            match host_secrets::get(name) {
                Ok(secret) => Ok(format!("SUCCESS: {secret}")),
                Err(host_secrets::SecretsError::NotFound) => Ok("DENIED: not-found".to_string()),
                Err(host_secrets::SecretsError::Denied) => Ok("DENIED: denied".to_string()),
                Err(host_secrets::SecretsError::Backend) => Ok("DENIED: backend".to_string()),
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
