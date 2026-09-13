//! Host-side state and capability implementations.
//!
//! Each extension instance gets its own [`HostState`] and Wasmtime store, so
//! `host-config` scopes to its section and `host-log` is tagged with its id.
//! These are the host half of `provider-world` imports (see [`crate::bindings`]).

use std::fmt::Write as _;

use serde_json::Value;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::jan_klod::interfaces::{host_config, host_http, host_log};

/// Extension instance's slice of `config.yaml`, served via `host-config`.
/// Keys are dot-separated paths into the (env-expanded) config.
#[derive(Debug, Clone)]
pub struct ConfigSection {
    root: Value,
}

impl ConfigSection {
    /// Wrap a resolved config object.
    #[must_use]
    pub const fn new(root: Value) -> Self {
        Self { root }
    }

    /// Dot-separated key path to JSON value.
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<&Value> {
        let mut cur = &self.root;
        for segment in key.split('.') {
            cur = cur.get(segment)?;
        }
        Some(cur)
    }

    /// JSON value at `key`, or `None` if absent.
    pub fn get(&self, key: &str) -> Option<String> {
        self.lookup(key).map(Value::to_string)
    }

    /// Whether `key` exists.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.lookup(key).is_some()
    }

    /// Entire section as JSON.
    #[must_use]
    pub fn all(&self) -> String {
        self.root.to_string()
    }
}

/// Per-instance store data: WASI context and host capabilities.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    /// Extension id (e.g., `provider.openai`) — tags log lines.
    component_id: String,
    /// Instance config section.
    section: ConfigSection,
    /// Outbound destinations via `host-http`. Per-state so the boundary
    /// is visible: guest calls consult the policy built for them.
    egress: crate::egress::EgressPolicy,
}

impl HostState {
    /// Build host state for one instance (public destinations only).
    pub fn new(component_id: impl Into<String>, section: ConfigSection) -> Self {
        Self {
            wasi: WasiCtxBuilder::new().inherit_stderr().build(),
            table: ResourceTable::new(),
            component_id: component_id.into(),
            section,
            egress: crate::egress::EgressPolicy::public_only(),
        }
    }

    /// Grant this instance operator-configured destinations.
    #[must_use]
    pub fn with_egress(mut self, egress: crate::egress::EgressPolicy) -> Self {
        self.egress = egress;
        self
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl host_log::Host for HostState {
    fn log(
        &mut self,
        level: host_log::LogLevel,
        component: String,
        message: String,
        fields: Vec<host_log::LogField>,
    ) {
        let level = match level {
            host_log::LogLevel::Debug => "DEBUG",
            host_log::LogLevel::Info => "INFO",
            host_log::LogLevel::Warn => "WARN",
            host_log::LogLevel::Error => "ERROR",
        };
        let mut line = format!("{level} [{}] {component}: {message}", self.component_id);
        for field in fields {
            // Write to String is infallible.
            let _ = write!(line, " {}={}", field.key, field.value);
        }
        eprintln!("{line}");
    }
}

impl host_config::Host for HostState {
    fn get(&mut self, key: String) -> Result<String, host_config::ConfigError> {
        self.section
            .get(&key)
            .ok_or(host_config::ConfigError::KeyNotFound)
    }

    fn has(&mut self, key: String) -> bool {
        self.section.has(&key)
    }

    fn all(&mut self) -> Result<String, host_config::ConfigError> {
        Ok(self.section.all())
    }
}

impl host_http::Host for HostState {
    /// Outbound HTTP: adapt types to the neutral blocking client (ureq, rustls).
    /// Only network access an extension gets.
    fn fetch(
        &mut self,
        request: host_http::HttpRequest,
    ) -> Result<host_http::HttpResponse, host_http::HttpError> {
        let headers: Vec<(String, String)> = request
            .headers
            .into_iter()
            .map(|h| (h.name, h.value))
            .collect();
        let result = crate::http::fetch_within(
            &self.egress,
            &request.method,
            &request.url,
            &headers,
            request.body.as_deref(),
            request.timeout_ms,
        );
        match result {
            Ok(response) => Ok(host_http::HttpResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| host_http::HttpHeader { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(err) => Err(to_http_error(&err)),
        }
    }
}

/// Translate [`crate::http::WireError`] to generated `host-http` error.
const fn to_http_error(err: &crate::http::WireError) -> host_http::HttpError {
    use crate::http::WireError;
    match err {
        WireError::InvalidUrl => host_http::HttpError::InvalidUrl,
        WireError::ConnectionFailed => host_http::HttpError::ConnectionFailed,
        WireError::Timeout => host_http::HttpError::Timeout,
        WireError::TlsError => host_http::HttpError::TlsError,
        WireError::ClientError(code) => host_http::HttpError::ClientError(*code),
        WireError::ServerError(code) => host_http::HttpError::ServerError(*code),
        WireError::Backend => host_http::HttpError::Backend,
    }
}
