//! Blocking outbound HTTP, the host half of `host-http`, over plain types so every bindgen surface
//! shares it. [`ureq`] keeps Wasmtime sync; 4xx/5xx surface as errors per the `host-http` contract.

use std::net::SocketAddr;
use std::time::Duration;

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::DefaultConnector;

/// Timeout applied when the caller passes `0` (`host-http`'s documented 30 s).
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

/// Classify a completed status: `4xx`/`5xx` become errors.
const fn status_error(status: u16) -> Option<WireError> {
    match status {
        400..=499 => Some(WireError::ClientError(status)),
        500..=599 => Some(WireError::ServerError(status)),
        _ => None,
    }
}

/// Perform one blocking HTTP request and read the full response into memory.
///
/// A `timeout_ms` of `0` selects [`DEFAULT_TIMEOUT_MS`].
///
/// # Errors
/// A [`WireError`]: a transport failure or, for a non-2xx/3xx status, `ClientError`/`ServerError`.
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

/// How many redirects to follow before giving up (ureq's former default).
const MAX_REDIRECTS: usize = 10;

/// Headers that must not survive a redirect.
///
/// Forwarding the caller's headers verbatim would leak a provider's API key to the redirect target;
/// ureq stripped `Authorization` itself, so the hand-rolled loop must. Dropped on every hop.
const STRIPPED_ON_REDIRECT: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// As [`fetch`], but every destination, including redirect hops, must satisfy `policy`.
///
/// This is what guests get; [`fetch`] applies the default public-only rule for the host's own calls,
/// so no spelling of "send anywhere" exists.
///
/// Redirects are followed here (`max_redirects(0)` on the agent) so policy is checked per hop: ureq
/// would otherwise follow a permitted origin's `302` to cloud metadata unchecked
/// ([#107](https://github.com/PromptPasture/jan-klod/issues/107)). Relative `Location`s resolve via
/// `url::Url` so the checked URL is the fetched one.
///
/// Each hop connects to the addresses the policy classified, so the name is resolved once
/// ([#108](https://github.com/PromptPasture/jan-klod/issues/108)).
///
/// # Errors
/// The policy's refusal for any hop, [`WireError::InvalidUrl`] for an unresolvable `Location`,
/// [`WireError::Backend`] beyond [`MAX_REDIRECTS`], or any [`WireError`] from an exchange.
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

/// The `Location` of a redirect response, if it is one.
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

/// `location` resolved against `base`, absolute or relative.
fn resolve(base: &str, location: &str) -> Result<String, WireError> {
    let base = url::Url::parse(base).map_err(|_| WireError::InvalidUrl)?;
    base.join(location)
        .map(|resolved| resolved.to_string())
        .map_err(|_| WireError::InvalidUrl)
}

/// ureq's cap on resolver addresses (`unversioned::resolver::MAX_ADDRS`, not exported; exceeding it
/// panics). Truncating is safe: the policy approves only if every address is public.
const MAX_PINNED_ADDRS: usize = 16;

/// A resolver that answers with the addresses the egress policy already classified, ignoring the name.
///
/// Fixes [#108](https://github.com/PromptPasture/jan-klod/issues/108): otherwise the host is looked up
/// twice and a rebinding name is checked as public, connected as private.
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

/// One request, no policy or redirect handling, sent to what `destination` says the policy approved.
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

/// Map a non-status `ureq` error onto [`WireError`]; unmodelled variants (it is `#[non_exhaustive]`) fold into `Backend`.
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
