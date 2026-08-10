//! Where a sandboxed component is allowed to send a request.
//!
//! `host-http` used to hand every granted guest an unrestricted client. The only
//! destination check in the runtime lived *inside* `tool-fetch`, which refuses
//! private and loopback addresses before calling out — and that is not a
//! boundary. It runs in the sandbox. It protects a confused model from a URL the
//! model itself chose; it does nothing about the component, which could simply
//! not perform the check. "The core trusts nothing it runs" and "the guest
//! validates its own egress" cannot both be true.
//!
//! What that left reachable, for any component granted the network:
//!
//! - `127.0.0.1:8787` — this gateway's own REST surface. Unauthenticated when no
//!   token is set, which is the default for loopback. A tool could drive the
//!   agent that is running it.
//! - `169.254.169.254` — the cloud metadata endpoint on EC2, GCE and Azure, which
//!   hands out IAM credentials to anything that asks.
//! - Everything else on the machine and the LAN: databases, admin panels, another
//!   jan-klod, the router.
//!
//! So the policy moves host-side. The default is **public destinations only**;
//! loopback, private, link-local and unique-local addresses are refused unless
//! the operator named that origin.
//!
//! ## Why naming origins, and not a flag
//!
//! Self-hosting means local models. `http://127.0.0.1:11434` is exactly where
//! Ollama lives, and refusing it would make the private-by-default runtime unable
//! to talk to the private-by-default model. A boolean "allow local" would open
//! every local port to every granted guest to solve that.
//!
//! Instead the allowance is per-origin, and the operator has already written the
//! origins down: a provider's `base-url`, an MCP server's endpoint. Those are
//! lifted from config into the policy, so the local Ollama a user configured is
//! reachable and the local Postgres they did not is not.
//!
//! ## What this does not do
//!
//! It resolves the hostname and checks every address it gets, so a public name
//! pointing at `127.0.0.1` is caught. It cannot close the window between that
//! check and the connection — a name that resolves differently on the second
//! lookup (DNS rebinding) would slip through. Closing that needs the resolved
//! address pinned into the connection itself, which `ureq` does not expose.
//! Recorded rather than hidden: it is a narrower hole than the one it replaces,
//! and an operator who cares can bind their local services to a unix socket.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use crate::http::WireError;

/// Destinations a component may reach.
#[derive(Clone, Debug, Default)]
pub struct EgressPolicy {
    /// Origins (`scheme://host:port`, lowercased) allowed even when they resolve
    /// somewhere private.
    allowed: HashSet<String>,
}

impl EgressPolicy {
    /// A policy allowing only public destinations.
    #[must_use]
    pub fn public_only() -> Self {
        Self::default()
    }

    /// Allow `url`'s origin — its scheme, host and port — regardless of where it
    /// resolves. The path is ignored: a grant is for an endpoint, not a route.
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

    /// Decide whether a request to `url` may leave the host.
    ///
    /// # Errors
    /// [`WireError::InvalidUrl`] if the URL has no host this can reason about,
    /// and [`WireError::ConnectionFailed`] when the destination is private and
    /// unallowed — deliberately the same error a refused connection produces, so
    /// a component cannot use the gate as a port scanner that distinguishes
    /// "blocked" from "nothing listening".
    pub fn check(&self, url: &str) -> Result<(), WireError> {
        let Some(origin) = origin_of(url) else {
            return Err(WireError::InvalidUrl);
        };
        if self.allowed.contains(&origin) {
            return Ok(());
        }
        let Some((host, port)) = host_port(url) else {
            return Err(WireError::InvalidUrl);
        };

        // An address literal needs no lookup — and must not get one, since a
        // resolver is exactly what an attacker would like us to consult.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if is_public(ip) { Ok(()) } else { Err(WireError::ConnectionFailed) };
        }

        // A name: every address it answers with has to be public. One private
        // answer is enough to refuse — a round-robin that sometimes points home
        // is not a destination this should reach on alternate attempts.
        let resolved = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| WireError::ConnectionFailed)?;
        let mut any = false;
        for addr in resolved {
            any = true;
            if !is_public(addr.ip()) {
                return Err(WireError::ConnectionFailed);
            }
        }
        if any {
            Ok(())
        } else {
            Err(WireError::ConnectionFailed)
        }
    }
}

/// `scheme://host:port` for `url`, lowercased, with the scheme's default port
/// filled in so `https://api.example.test` and `https://api.example.test:443`
/// are one origin.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // Credentials in the authority are not part of an origin.
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

/// Split `host[:port]`, handling the `[::1]:8080` bracket form, and default the
/// port from the scheme.
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

/// Whether an address is one a sandboxed component may reach without a grant.
///
/// Everything reserved for the local machine, a private network, or the
/// link-local range is refused — the last of these because `169.254.169.254` is
/// the cloud metadata service, and reaching it is a credential leak rather than
/// a connection.
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
        // Carrier-grade NAT (100.64.0.0/10) — an ISP's internal space.
        || (a == 100 && (64..=127).contains(&b))
        // Benchmarking (198.18.0.0/15) and reserved (240.0.0.0/4).
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    // An IPv4-mapped address is an IPv4 destination wearing a hat.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let first = ip.segments()[0];
    // fc00::/7 unique-local, fe80::/10 link-local.
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
        assert_eq!(policy.check("http://[::ffff:127.0.0.1]/"), Err(WireError::ConnectionFailed));
    }

    #[test]
    fn a_configured_local_endpoint_is_reachable() {
        // The case this exists for: a self-hosted model on loopback.
        let policy = EgressPolicy::public_only().allowing("http://127.0.0.1:11434/v1");
        assert_eq!(policy.check("http://127.0.0.1:11434/v1/chat/completions"), Ok(()));
        // The grant is that origin, not the machine: another local port stays shut.
        assert_eq!(
            policy.check("http://127.0.0.1:5432/"),
            Err(WireError::ConnectionFailed),
            "allowing Ollama must not allow the database next to it"
        );
        // Nor a different scheme on the same port.
        assert_eq!(
            policy.check("https://127.0.0.1:11434/v1"),
            Err(WireError::ConnectionFailed)
        );
    }

    #[test]
    fn a_default_port_matches_an_explicit_one() {
        let policy = EgressPolicy::public_only().allowing("https://api.example.test");
        assert_eq!(policy.check("https://api.example.test:443/v1/chat"), Ok(()));
        let policy = EgressPolicy::public_only().allowing("http://192.168.0.9:80/mcp");
        assert_eq!(policy.check("http://192.168.0.9/mcp/tools"), Ok(()));
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
        assert_eq!(policy.check("https://93.184.216.34/"), Ok(()));
    }
}
