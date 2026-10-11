//! Blocking outbound HTTP — the host half of the `host-http` capability.
//!
//! Kept capability-neutral (plain types, no generated bindings) so every
//! `host-http` bindgen surface can share one implementation: the core's
//! [`crate::host::HostState`] and `provider_probe` example both adapt their
//! generated request/response types to the functions here.
//!
//! [`ureq`] gives a synchronous client with rustls TLS and no async runtime,
//! keeping sync Wasmtime baseline intact. Per the `host-http` contract, 4xx/5xx
//! are surfaced as errors; transport failures collapse onto the matching
//! variant.

use std::net::SocketAddr;
use std::time::Duration;

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::DefaultConnector;

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

/// How many redirects to follow before giving up.
///
/// ureq's own former default, now enforced here because this module drives the
/// redirect loop itself — see [`fetch_within`].
const MAX_REDIRECTS: usize = 10;

/// Headers that must not survive a redirect.
///
/// **This is why following redirects by hand is not merely more code.** ureq
/// defaults to `RedirectAuthHeaders::Never` and strips `Authorization` for you;
/// a loop that forwarded the caller's headers verbatim would send a provider's
/// API key to whatever the redirect pointed at — a credential leak introduced
/// by the fix for a policy bypass. Dropped on *every* hop rather than only
/// cross-origin ones, matching what ureq did before.
const STRIPPED_ON_REDIRECT: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// As [`fetch`], but every destination must satisfy `policy` — including
/// redirect destinations.
///
/// This is the function the runtime hands to guests. [`fetch`] keeps the
/// unparameterised signature for the host's own calls (probe example, `ask`
/// CLI) and applies default public-only rule, so no spelling of "send
/// anywhere" is left in the codebase.
///
/// # Redirects are followed here, not by ureq
///
/// `max_redirects(0)` on the agent with a loop around it, because the policy
/// must be checked **per hop**. ureq follows up to ten redirects silently, so
/// a permitted origin answering `302 Location: http://169.254.169.254/…` would
/// reach cloud metadata with policy checked only on the first URL
/// ([#107](https://github.com/PromptPasture/jan-klod/issues/107)). The
/// mattering check is on the destination actually contacted.
///
/// A relative `Location` is resolved against the current URL by `url::Url`
/// rather than by hand: RFC 3986's rules are not obvious, and getting them
/// wrong here would mean checking one URL and fetching another.
///
/// # The hop goes to the address that was checked
///
/// Checking a hop is only worth something if the connection lands where the
/// check looked. The policy hands back the addresses it classified and
/// [`exchange`] connects to those, so the name is resolved once
/// ([#108](https://github.com/PromptPasture/jan-klod/issues/108)).
///
/// # Errors
/// The policy's refusal (see [`crate::egress::EgressPolicy::check`]) for any
/// hop, [`WireError::InvalidUrl`] for a `Location` that cannot be resolved,
/// [`WireError::Backend`] if the chain exceeds [`MAX_REDIRECTS`], or any
/// [`WireError`] from an exchange itself.
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
        // Every hop, including the first. This is the whole point of the loop.
        // The hop is then made to what the check resolved, not to what a second
        // lookup of the same name would say — see [`exchange`].
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

        // A 3xx that names somewhere else: resolve it, strip what must not
        // travel, and let the top of the loop decide whether it is permitted.
        target = resolve(&target, &location)?;
        forwarded.retain(|(name, _)| {
            let name = name.to_ascii_lowercase();
            !STRIPPED_ON_REDIRECT.contains(&name.as_str())
        });
        // 303 is defined to become a GET, and 301/302 have done so in practice
        // for so long that preserving a POST across them would surprise every
        // caller. 307 and 308 exist precisely to preserve it, so they do.
        if matches!(response.status, 301..=303)
            && !method.eq_ignore_ascii_case("GET")
            && !method.eq_ignore_ascii_case("HEAD")
        {
            "GET".clone_into(&mut method);
            body = None;
        }
    }
    // Refusing to keep going is the safe end of a redirect loop: a chain that
    // long is either a mistake or someone probing for one.
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

/// ureq's cap on how many addresses a resolver may answer with
/// (`unversioned::resolver::MAX_ADDRS`, not exported). Pushing past it panics,
/// so the pin is truncated here. Safe to truncate: the policy refuses the URL
/// unless **every** address it resolved is public, so any subset it approved is
/// a subset of addresses it approved.
const MAX_PINNED_ADDRS: usize = 16;

/// A resolver that answers with the addresses the egress policy already
/// classified, ignoring the name entirely.
///
/// This is the fix for [#108](https://github.com/PromptPasture/jan-klod/issues/108):
/// without it the host is looked up twice — once by
/// [`crate::egress::EgressPolicy::check`] and once by ureq — and a name whose
/// answer changes in between is checked as public and connected as private.
#[derive(Debug)]
struct Pinned(Vec<SocketAddr>);

impl Resolver for Pinned {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        // The URI is deliberately unused. Consulting it is the second lookup.
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

/// One request, with no policy and no redirect handling, sent to what
/// `destination` says the policy approved.
fn exchange(
    method: &str,
    url: &str,
    destination: &crate::egress::Destination,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: u32,
) -> Result<WireResponse, WireError> {
    // One-shot agent per request: simple, providers issue infrequent calls.
    // `http_status_as_error(false)` reads 4xx/5xx as responses, mapping them
    // ourselves rather than losing the status inside a ureq error.
    //
    // **`max_redirects(0)` is a security setting, not a preference.** ureq
    // follows up to 10 redirects silently, leaving egress policy consulted only
    // on the first URL. At 0 the 3xx is returned as-is (`max_redirects_do_error()`
    // is `max_redirects > 0 && …`), letting `fetch_within` check each hop.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(u64::from(timeout))))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();

    // `Host` and TLS are unaffected by pinning: ureq takes SNI and header from
    // the URI's authority, never from the connection address, so virtual
    // hosting and certificate validation see the requested name.
    //
    // The two arms differ in the resolver only: `Agent::from(config)` is
    // `with_parts(config, DefaultConnector::default(), DefaultResolver)`, so
    // the pinned agent keeps the same transport, TLS and pooling as before.
    //
    // **`Agent::with_parts` and `Resolver` live in `ureq::unversioned`,
    // excluded from its own semver docs.** A ureq minor bump may break this
    // site; that's a compile error not a silent #108 reopening, the price of
    // the guarantee.
    let agent: ureq::Agent = match destination {
        crate::egress::Destination::Resolved(addrs) => {
            ureq::Agent::with_parts(config, DefaultConnector::default(), Pinned(addrs.clone()))
        }
        // Nothing was resolved, so there is nothing to pin — an address literal
        // has no lookup to rebind, and a granted origin was trusted by name.
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

    // No status mapping here: `fetch_within` needs to see a 3xx to follow it,
    // and it applies `status_error` to the response it finally returns.
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
    use ureq::unversioned::resolver::Resolver;
    use ureq::unversioned::transport::{time::Duration, NextTimeout};

    use super::{status_error, Pinned, WireError};

    #[test]
    fn the_pinned_resolver_ignores_the_name_it_is_asked_about() {
        // The whole of #108 in one assertion: whatever the URI says, the answer
        // is the address the policy classified. A resolver that consulted the
        // name here would be the second lookup that rebinding needs.
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
