//! Where a sandboxed component is allowed to send a request.
//!
//! Destination checking must live host-side. Guest-side checks (e.g. `tool-fetch`
//! refusing private addresses) protect a confused model, not the host from a
//! component that skips them. Default: **public destinations only**; loopback,
//! private, link-local and unique-local are refused unless the operator named
//! the origin.
//!
//! ## Why per-origin grants, not a boolean flag
//!
//! Self-hosted local models (Ollama on `127.0.0.1:11434`) need to reach private
//! endpoints. A "allow all local" flag opens every local port to every guest.
//! Per-origin grants reuse the config the operator already wrote (provider
//! `base-url`, MCP server endpoint): the Ollama they configured is reachable,
//! the Postgres they didn't is not.
//!
//! ## One DNS lookup, not two
//!
//! Checks every address a hostname resolves to; a public name pointing at
//! `127.0.0.1` is caught. Addresses are handed back (not dropped) so the caller
//! connects to the same addresses the check approved. This prevents DNS rebinding
//! ([#108](https://github.com/PromptPasture/jan-klod/issues/108)): a name whose
//! answer changes between check and connect is checked as public but connected to
//! as private.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use crate::http::WireError;

/// Where a permitted request may go — what [`EgressPolicy::check`] decided.
///
/// Returned instead of `()` to pin the addresses the policy classified, preventing
/// a second lookup (see module docs for DNS rebinding defense).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Hostname resolved; every answer was public. Connect to these addresses
    /// (prevents re-resolution).
    Resolved(Vec<SocketAddr>),
    /// No resolution needed: either the URL named an address literal, or the
    /// operator granted this origin.
    AsNamed,
}

/// Destinations a component may reach.
#[derive(Clone, Debug, Default)]
pub struct EgressPolicy {
    /// Origins (`scheme://host:port`, lowercased) allowed despite resolving private.
    allowed: HashSet<String>,
}

impl EgressPolicy {
    /// A policy allowing only public destinations.
    #[must_use]
    pub fn public_only() -> Self {
        Self::default()
    }

    /// Allow `url`'s origin (scheme, host, port) regardless of where it resolves.
    /// Path is ignored.
    #[must_use]
    pub fn allowing(mut self, url: &str) -> Self {
        if let Some(origin) = origin_of(url) {
            self.allowed.insert(origin);
        }
        self
    }

    /// Whether anything has been explicitly allowed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Decide if a request to `url` may leave the host.
    ///
    /// # Errors
    /// [`WireError::InvalidUrl`] if the URL has no usable host. [`WireError::ConnectionFailed`]
    /// when the destination is private and unallowed — intentionally the same error as a
    /// refused connection, so components cannot scan ports by distinguishing "blocked" from
    /// "nothing listening".
    pub fn check(&self, url: &str) -> Result<Destination, WireError> {
        let Some(origin) = origin_of(url) else {
            return Err(WireError::InvalidUrl);
        };
        if self.allowed.contains(&origin) {
            return Ok(Destination::AsNamed);
        }
        let Some((host, port)) = host_port(url) else {
            return Err(WireError::InvalidUrl);
        };

        // Address literals need no lookup and must not get one (resolver is an attack vector).
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if is_public(ip) {
                Ok(Destination::AsNamed)
            } else {
                Err(WireError::ConnectionFailed)
            };
        }

        // Hostname: all resolved addresses must be public; even one private address
        // (e.g., round-robin alternate) causes refusal.
        let resolved = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| WireError::ConnectionFailed)?;
        let mut addrs = Vec::new();
        for addr in resolved {
            if !is_public(addr.ip()) {
                return Err(WireError::ConnectionFailed);
            }
            addrs.push(addr);
        }
        if addrs.is_empty() {
            return Err(WireError::ConnectionFailed);
        }
        // All classified as public; connecting to any of them is safe.
        Ok(Destination::Resolved(addrs))
    }
}

/// `scheme://host:port` for `url` (lowercased, scheme's default port filled in).
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // Credentials are not part of an origin.
    let authority = authority.rsplit('@').next()?;
    if authority.is_empty() {
        return None;
    }
    let (host, port) = split_authority(authority, &scheme)?;
    Some(format!("{scheme}://{host}:{port}"))
}

/// The host and port a URL names.
fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    split_authority(authority, &scheme)
}

/// Split `host[:port]` (handles `[::1]:8080` bracket form, defaults port from scheme).
fn split_authority(authority: &str, scheme: &str) -> Option<(String, u16)> {
    let default_port = if scheme == "https" { 443 } else { 80 };
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail
            .strip_prefix(':')
            .map_or(Some(default_port), |p| p.parse().ok())?;
        return Some((host.to_lowercase(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => Some((host.to_lowercase(), port.parse().ok()?)),
        _ => Some((authority.to_lowercase(), default_port)),
    }
}

/// Whether an address is reachable without a grant.
///
/// Rejects loopback, private, link-local, and reserved ranges. Link-local
/// exclusion prevents access to cloud metadata (`169.254.169.254`).
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || a == 0
        // Carrier-grade NAT (ISP internal: 100.64.0.0/10)
        || (a == 100 && (64..=127).contains(&b))
        // Benchmarking (198.18.0.0/15) and reserved (240.0.0.0/4)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    // IPv4-mapped addresses are checked as IPv4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let first = ip.segments()[0];
    // Reject unique-local (fc00::/7) and link-local (fe80::/10).
    !((first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_private_addresses_are_refused() {
        let policy = EgressPolicy::public_only();
        for url in [
            "http://127.0.0.1:8787/session/a/message",
            "http://localhost:8787/",
            "http://[::1]:8787/",
            "http://10.0.0.5/admin",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
            "http://0.0.0.0/",
            "http://100.100.100.200/",
        ] {
            assert_eq!(
                policy.check(url),
                Err(WireError::ConnectionFailed),
                "{url} must not be reachable without a grant"
            );
        }
    }

    #[test]
    fn an_ipv4_mapped_ipv6_loopback_is_still_loopback() {
        let policy = EgressPolicy::public_only();
        assert_eq!(
            policy.check("http://[::ffff:127.0.0.1]/"),
            Err(WireError::ConnectionFailed)
        );
    }

    #[test]
    fn a_configured_local_endpoint_is_reachable() {
        // Self-hosted model on loopback: grant is per-origin only.
        let policy = EgressPolicy::public_only().allowing("http://127.0.0.1:11434/v1");
        assert_eq!(
            policy.check("http://127.0.0.1:11434/v1/chat/completions"),
            Ok(Destination::AsNamed)
        );
        // Different port on same host stays blocked.
        assert_eq!(
            policy.check("http://127.0.0.1:5432/"),
            Err(WireError::ConnectionFailed),
            "allowing Ollama must not allow the database next to it"
        );
        // Different scheme on same port stays blocked.
        assert_eq!(
            policy.check("https://127.0.0.1:11434/v1"),
            Err(WireError::ConnectionFailed)
        );
    }

    #[test]
    fn a_default_port_matches_an_explicit_one() {
        // https default (443) and http default (80) are normalized.
        let policy = EgressPolicy::public_only().allowing("https://api.example.test");
        assert_eq!(
            policy.check("https://api.example.test:443/v1/chat"),
            Ok(Destination::AsNamed)
        );
        let policy = EgressPolicy::public_only().allowing("http://192.168.0.9:80/mcp");
        assert_eq!(
            policy.check("http://192.168.0.9/mcp/tools"),
            Ok(Destination::AsNamed)
        );
    }

    #[test]
    fn non_http_schemes_and_junk_are_rejected() {
        let policy = EgressPolicy::public_only();
        for url in ["file:///etc/passwd", "gopher://x/", "not a url", ""] {
            assert_eq!(policy.check(url), Err(WireError::InvalidUrl), "{url}");
        }
    }

    #[test]
    fn credentials_in_the_authority_do_not_change_the_origin() {
        // `http://public.test@127.0.0.1/` points at 127.0.0.1, not public.test.
        let policy = EgressPolicy::public_only().allowing("http://public.test");
        assert_eq!(
            policy.check("http://public.test@127.0.0.1/"),
            Err(WireError::ConnectionFailed),
            "the userinfo is not the host"
        );
    }

    #[test]
    fn a_public_address_is_allowed_without_a_grant() {
        let policy = EgressPolicy::public_only();
        // A literal, so there is no lookup to pin and nothing to rebind.
        assert_eq!(
            policy.check("https://93.184.216.34/"),
            Ok(Destination::AsNamed)
        );
    }

    #[test]
    fn a_resolved_name_hands_back_the_addresses_it_classified() {
        // Issue #108: prevent DNS rebinding. Unapproved names are resolved
        // and checked; approved origins skip resolution.
        let policy = EgressPolicy::public_only();
        assert_eq!(
            policy.check("http://localhost:8787/"),
            Err(WireError::ConnectionFailed),
            "ungranted localhost resolves to private and is refused"
        );

        let policy = EgressPolicy::public_only().allowing("http://localhost:8787");
        assert_eq!(
            policy.check("http://localhost:8787/v1"),
            Ok(Destination::AsNamed),
            "granted origin skips resolution, client looks it up itself"
        );
    }
}
