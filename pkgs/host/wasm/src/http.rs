//! Blocking outbound HTTP, the host half of `host-http`, over plain types shared by every bindgen surface.

use std::net::SocketAddr;
use std::time::Duration;

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::DefaultConnector;

/// Applied when the caller passes `0`.
const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// A completed HTTP exchange, free of generated types.
pub struct WireResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Raw response body.
    pub body: Vec<u8>,
}

/// The `host-http` `http-error` variants without a generated type.
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

const fn status_error(status: u16) -> Option<WireError> {
    match status {
        400..=499 => Some(WireError::ClientError(status)),
        500..=599 => Some(WireError::ServerError(status)),
        _ => None,
    }
}

/// One blocking HTTP request, read fully into memory; a `timeout_ms` of `0` selects the default.
///
/// # Errors
/// A [`WireError`]: transport failure, or `ClientError`/`ServerError` for a non-2xx/3xx status.
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

const MAX_REDIRECTS: usize = 10;

/// Dropped on every hop: forwarding them would leak a provider's API key to the redirect target.
const STRIPPED_ON_REDIRECT: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// As [`fetch`], but every destination, including redirect hops, must satisfy `policy`.
///
/// # Errors
/// The policy's refusal for any hop, or a [`WireError`] from an exchange or too many redirects.
pub fn fetch_within(
    policy: &crate::egress::EgressPolicy,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout_ms: u32,
) -> Result<WireResponse, WireError> {
    let timeout = if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        timeout_ms
    };
    let mut target = url.to_owned();
    let mut method = method.to_owned();
    let mut body = body.map(<[u8]>::to_vec);
    let mut forwarded = headers.to_vec();

    for _ in 0..=MAX_REDIRECTS {
        let destination = policy.check(&target)?;
        let response = exchange(
            &method,
            &target,
            &destination,
            &forwarded,
            body.as_deref(),
            timeout,
        )?;

        let Some(location) = redirect_location(&response) else {
            if let Some(err) = status_error(response.status) {
                return Err(err);
            }
            return Ok(response);
        };

        target = resolve(&target, &location)?;
        forwarded.retain(|(name, _)| {
            let name = name.to_ascii_lowercase();
            !STRIPPED_ON_REDIRECT.contains(&name.as_str())
        });
        if matches!(response.status, 301..=303)
            && !method.eq_ignore_ascii_case("GET")
            && !method.eq_ignore_ascii_case("HEAD")
        {
            "GET".clone_into(&mut method);
            body = None;
        }
    }
    Err(WireError::Backend)
}

fn redirect_location(response: &WireResponse) -> Option<String> {
    if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn resolve(base: &str, location: &str) -> Result<String, WireError> {
    let base = url::Url::parse(base).map_err(|_| WireError::InvalidUrl)?;
    base.join(location)
        .map(|resolved| resolved.to_string())
        .map_err(|_| WireError::InvalidUrl)
}

/// ureq's cap on resolver addresses (not exported; exceeding it panics).
const MAX_PINNED_ADDRS: usize = 16;

/// Resolves to the addresses the policy classified, so a rebinding name cannot change between check and connect.
#[derive(Debug)]
struct Pinned(Vec<SocketAddr>);

impl Resolver for Pinned {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = self.empty();
        for addr in self.0.iter().take(MAX_PINNED_ADDRS) {
            addrs.push(*addr);
        }
        if addrs.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(addrs)
    }
}

fn exchange(
    method: &str,
    url: &str,
    destination: &crate::egress::Destination,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: u32,
) -> Result<WireResponse, WireError> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(u64::from(timeout))))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();

    let agent: ureq::Agent = match destination {
        crate::egress::Destination::Resolved(addrs) => {
            ureq::Agent::with_parts(config, DefaultConnector::default(), Pinned(addrs.clone()))
        }
        crate::egress::Destination::AsNamed => config.into(),
    };

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

    Ok(WireResponse {
        status,
        headers,
        body,
    })
}

/// Unmodelled variants (`ureq::Error` is `#[non_exhaustive]`) fold into `Backend`.
const fn map_transport(err: &ureq::Error) -> WireError {
    match err {
        ureq::Error::Timeout(_) => WireError::Timeout,
        ureq::Error::HostNotFound | ureq::Error::Io(_) => WireError::ConnectionFailed,
        _ => WireError::Backend,
    }
}

#[cfg(test)]
mod tests {
    use ureq::unversioned::resolver::Resolver;
    use ureq::unversioned::transport::{time::Duration, NextTimeout};

    use super::{status_error, Pinned, WireError};

    #[test]
    fn the_pinned_resolver_ignores_the_name_it_is_asked_about() {
        let pinned = Pinned(vec!["93.184.216.34:443".parse().unwrap()]);
        let config = ureq::Agent::config_builder().build();
        let timeout = NextTimeout {
            after: Duration::from_secs(5),
            reason: ureq::Timeout::Resolve,
        };

        for uri in ["https://example.test/", "https://elsewhere.test/"] {
            let resolved = pinned
                .resolve(&uri.parse().unwrap(), &config, timeout)
                .expect("a pinned address always resolves");
            assert_eq!(
                &resolved[..],
                ["93.184.216.34:443".parse().unwrap()],
                "{uri} must not change where the connection goes"
            );
        }
    }

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
