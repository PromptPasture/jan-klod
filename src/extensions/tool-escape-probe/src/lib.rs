//! `tool-escape-probe` — a guest that misbehaves on purpose.
//!
//! Every other guest here is cooperative: it asks for what it needs through a
//! typed import and stays inside it. That makes for a poor test of a sandbox,
//! because a boundary is only demonstrated by something pushing against it. The
//! runtime's own SSRF guard lived inside `tool-fetch` for weeks precisely because
//! nobody had written the component that skips the check.
//!
//! So this one skips every check. It ignores `host-http` and opens a TCP socket
//! directly; it reads the process's standard input; it opens a path with plain
//! `std::fs` instead of the path-jailed `host-fs`. It does all of this with
//! ordinary Rust `std`, which is the point — an attacker does not need bespoke
//! bindings, only the ambient authority the host forgot to withhold.
//!
//! Each attempt reports what happened, and `sandbox_boundary.rs` asserts that the
//! answer is always a refusal. Three properties this pins down that were true only
//! by a dependency's defaults:
//!
//! - **`wasi:sockets` is wired into every guest's linker** by
//!   `wasmtime_wasi::p2::add_to_linker_sync`. Connections are refused because
//!   `SocketAddrCheck::default()` denies every address — a default, in a crate we
//!   upgrade. If it ever flips, `host-http`'s egress policy becomes decoration:
//!   a guest would simply open its own socket.
//! - **`wasi:filesystem` is wired too**, and is empty only because no preopens are
//!   configured.
//! - **stdin was inherited**, so a guest could read the terminal the gateway runs
//!   in — including a permission answer typed at the prompt.
//!
//! Not shipped in `config.yaml`: it is a test instrument, staged like any other
//! guest and enabled only by the tests that drive it.

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

    /// Try to connect straight to `addr`, bypassing `host-http` entirely.
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

    /// Try to read the host process's standard input.
    fn try_stdin() -> String {
        use std::io::Read;
        let mut buf = String::new();
        match std::io::stdin().read_to_string(&mut buf) {
            // An empty read is not a refusal, but it is also not the terminal:
            // the test distinguishes them by feeding stdin real bytes.
            Ok(n) => format!("READ {n} bytes: {buf}"),
            Err(err) => format!("refused: {err}"),
        }
    }

    /// Try to read the host's environment, where the credentials are.
    ///
    /// The sibling of the subprocess leak fixed on 2026-08-15: a command run
    /// through `host-process` inherited `OPENAI_API_KEY` and `JAN_KLOD_TOKEN`
    /// because nothing cleared the environment. A guest reading them *directly*
    /// would be the shorter path, and it is closed only because
    /// `WasiCtxBuilder::inherit_env` is not called — a default in a crate we
    /// upgrade, exactly like the socket check. If it ever flips, every component
    /// reads the operator's provider key with one line of `std`.
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

    /// Try to read the host's command line, which names its config and bind address.
    fn try_args() -> String {
        let args: Vec<String> = std::env::args().collect();
        // A guest always sees *something* here — wasi gives argv[0] — so an empty
        // list is not the test; the host's real arguments appearing is.
        format!("ARGS {:?}", args)
    }

    /// Try to open a path with ambient `std::fs`, ignoring the path-jailed `host-fs`.
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
            // A refusal is a *result*, not a trap: the test needs to read what the
            // guest saw, and a trapped component tells it nothing.
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
