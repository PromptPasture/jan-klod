//! Blocking outbound HTTP — the host half of the `host-http` capability.
//!
//! Kept capability-neutral (plain types, no generated bindings) so every
//! `host-http` bindgen surface can share one implementation: the core's own
//! [`crate::host::HostState`] and the `provider_probe` example both adapt their
//! generated request/response types to the functions here.
//!
//! [`ureq`] gives a synchronous client with rustls TLS and no async runtime,
//! keeping the sync Wasmtime baseline intact. Per the `host-http` contract,
//! 4xx/5xx are surfaced as errors; transport failures collapse onto the
//! matching variant.

use std::time::Duration;

/// Timeout applied when the caller passes `0` (mirrors `host-http`'s documented
/// 30 s host default).
const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// A completed HTTP exchange, free of any generated bindings type.
pub struct WireResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Raw response body.
    pub body: Vec<u8>,
}

/// The `host-http` `http-error` variants, expressed without a generated type so
/// both bindgen surfaces can map onto them.
#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    /// The URL could not be parsed / the method was rejected.
    InvalidUrl,
    /// DNS or TCP connection failure.
    ConnectionFailed,
    /// The request exceeded its timeout.
    Timeout,
    /// TLS negotiation failed.
    TlsError,
    /// HTTP 4xx — carries the status code.
    ClientError(u16),
    /// HTTP 5xx — carries the status code.
    ServerError(u16),
    /// Any other backend failure.
    Backend,
}

/// Classify a completed HTTP status: `4xx`/`5xx` become errors, everything else
/// is a success the caller keeps.
const fn status_error(status: u16) -> Option<WireError> {
    match status {
        400..=499 => Some(WireError::ClientError(status)),
        500..=599 => Some(WireError::ServerError(status)),
        _ => None,
    }
}

/// Perform one blocking HTTP request and read the full response into memory.
///
/// `headers` are `(name, value)` pairs; `body` is sent verbatim (omit for
/// bodyless methods). A `timeout_ms` of `0` selects [`DEFAULT_TIMEOUT_MS`].
///
/// # Errors
/// Returns a [`WireError`]: a transport failure ([`WireError::ConnectionFailed`],
/// [`WireError::Timeout`], …) or, for a completed exchange with a non-2xx/3xx
/// status, [`WireError::ClientError`] / [`WireError::ServerError`].
pub fn fetch(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout_ms: u32,
) -> Result<WireResponse, WireError> {
    fetch_within(
        &crate::egress::EgressPolicy::public_only(),
        method,
        url,
        headers,
        body,
        timeout_ms,
    )
}

/// As [`fetch`], but the destination must satisfy `policy` first.
///
/// This is the function the runtime hands to guests. [`fetch`] keeps the
/// unparameterised signature for the host's own calls (a probe example, the
/// `ask` CLI) and applies the default public-only rule, so there is no spelling
/// of "send anywhere" left in the codebase.
///
/// # Errors
/// The policy's refusal (see [`crate::egress::EgressPolicy::check`]) or any
/// [`WireError`] from the exchange itself.
pub fn fetch_within(
    policy: &crate::egress::EgressPolicy,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout_ms: u32,
) -> Result<WireResponse, WireError> {
    policy.check(url)?;
    let timeout = if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        timeout_ms
    };
    // One-shot agent per request: simple, and providers issue infrequent calls.
    // `http_status_as_error(false)` lets us read 4xx/5xx as responses and map
    // them ourselves rather than losing the status inside a ureq error.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(u64::from(timeout))))
        .http_status_as_error(false)
        .build()
        .into();

    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(body.unwrap_or_default().to_vec())
        .map_err(|_| WireError::InvalidUrl)?;

    let mut response = agent.run(request).map_err(|err| map_transport(&err))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .body_mut()
        .read_to_vec()
        .map_err(|_| WireError::Backend)?;

    if let Some(err) = status_error(status) {
        return Err(err);
    }
    Ok(WireResponse {
        status,
        headers,
        body,
    })
}

/// Map a non-status `ureq` error onto the neutral [`WireError`]. The variant set
/// is `#[non_exhaustive]`, so anything unmodelled folds into
/// [`WireError::Backend`].
const fn map_transport(err: &ureq::Error) -> WireError {
    match err {
        ureq::Error::Timeout(_) => WireError::Timeout,
        ureq::Error::HostNotFound | ureq::Error::Io(_) => WireError::ConnectionFailed,
        _ => WireError::Backend,
    }
}

#[cfg(test)]
mod tests {
    use super::{status_error, WireError};

    #[test]
    fn success_statuses_are_not_errors() {
        assert_eq!(status_error(200), None);
        assert_eq!(status_error(204), None);
        assert_eq!(status_error(302), None);
    }

    #[test]
    fn client_and_server_statuses_map_to_errors() {
        assert_eq!(status_error(400), Some(WireError::ClientError(400)));
        assert_eq!(status_error(404), Some(WireError::ClientError(404)));
        assert_eq!(status_error(429), Some(WireError::ClientError(429)));
        assert_eq!(status_error(500), Some(WireError::ServerError(500)));
        assert_eq!(status_error(503), Some(WireError::ServerError(503)));
    }
}
