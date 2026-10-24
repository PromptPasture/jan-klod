//! `tool-escape-probe`: a guest that deliberately misbehaves; `sandbox_boundary.rs` asserts every attempt is refused.

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

    struct Component;

    impl Lifecycle for Component {
        fn init(ctx: ExtensionContext) -> Result<(), String> {
            host_log::log(
                LogLevel::Info,
                "tool-escape-probe",
                &format!(
                    "init id={} — this guest attempts to escape on purpose",
                    ctx.id
                ),
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

    fn try_socket(addr: &str) -> String {
        use std::io::Write;
        match std::net::TcpStream::connect(addr) {
            Ok(mut stream) => {
                let wrote = stream.write_all(b"GET / HTTP/1.0\r\n\r\n").is_ok();
                format!("CONNECTED to {addr} (wrote={wrote})")
            }
            Err(err) => format!("refused: {err}"),
        }
    }

    fn try_stdin() -> String {
        use std::io::Read;
        let mut buf = String::new();
        match std::io::stdin().read_to_string(&mut buf) {
            Ok(n) => format!("READ {n} bytes: {buf}"),
            Err(err) => format!("refused: {err}"),
        }
    }

    /// Closed only because `WasiCtxBuilder::inherit_env` is not called; if that flips, every guest reads provider keys.
    fn try_env() -> String {
        let named: Vec<String> = [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "JAN_KLOD_TOKEN",
            "HOME",
        ]
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| format!("{key}={value}"))
        })
        .collect();
        let total = std::env::vars().count();
        if named.is_empty() && total == 0 {
            "refused: the environment is empty".to_string()
        } else {
            format!("READ {total} vars: {}", named.join(" "))
        }
    }

    fn try_args() -> String {
        let args: Vec<String> = std::env::args().collect();
        format!("ARGS {args:?}")
    }

    fn try_fs(path: &str) -> String {
        match std::fs::read_to_string(path) {
            Ok(text) => format!(
                "READ {} bytes from {path}: {}",
                text.len(),
                &text[..text.len().min(40)]
            ),
            Err(err) => format!("refused: {err}"),
        }
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "escape-probe".to_string(),
                description: "Test instrument: attempts to leave the sandbox.".to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["socket", "stdin", "fs", "env", "args"] },
                        "target": { "type": "string" }
                    },
                    "required": ["op"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let op = value
                .get("op")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let target = value
                .get("target")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(match op {
                "socket" => try_socket(target),
                "stdin" => try_stdin(),
                "fs" => try_fs(target),
                "env" => try_env(),
                "args" => try_args(),
                other => format!("unknown op {other}"),
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
