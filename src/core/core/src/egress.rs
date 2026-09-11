//! Where a sandboxed component is allowed to send a request.
//!
//! Destination checking cannot live in the guest (e.g. `tool-fetch` refusing
//! private/loopback addresses before calling out): that protects a confused
//! model from a URL it chose itself, not the host from a component that skips
//! the check. Left unguarded, any networked guest could reach this gateway's own
//! REST API, the cloud metadata endpoint (`169.254.169.254`), or anything else on
//! the machine and LAN.
//!
//! So the policy lives host-side. Default is **public destinations only**;
//! loopback, private, link-local and unique-local addresses are refused unless
//! the operator named that origin.
//!
//! ## Why naming origins, not a flag
//!
//! Self-hosting means local models (Ollama on `127.0.0.1:11434`), so a
//! private-by-default runtime still needs to reach a private-by-default model. A
//! boolean "allow local" would open every local port to every granted guest.
//! Instead the allowance is per-origin, lifted from config the operator already
//! wrote (a provider's `base-url`, an MCP server's endpoint) — the local Ollama a
//! user configured is reachable, the local Postgres they didn't is not.
//!
//! ## One lookup, not two
//!
//! Resolves the hostname and checks every address returned, so a public name
//! pointing at `127.0.0.1` is caught. The addresses are then **handed back**
//! rather than dropped, so the caller connects to the ones that were
//! classified. Two separate resolutions is exactly what DNS rebinding needs: a
//! name whose answer changes in between is checked as public and connected to
//! as private ([#108](https://github.com/PromptPasture/jan-klod/issues/108)).
//! Deciding here and connecting somewhere else would leave all the care over
//! address classes below undone.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use crate::http::WireError;

/// Where a permitted request may go — what [`EgressPolicy::check`] decided.
///
/// Returned instead of `()` so the connection can be made to the addresses the
/// policy classified. See the module docs: the value exists to stop the name
/// being looked up a second time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// A name was resolved and every answer was public. Connect to **these**
    /// addresses — resolving the name again is the hole this closes.
    Resolved(Vec<SocketAddr>),
    /// Nothing was resolved, so there is nothing to pin. Either the URL named
    /// an address literal, which no resolver is consulted for and no answer can
    /// change, or the operator granted this origin — where a granted origin
    /// points is their decision, not a classification this made.
    AsNamed,
}

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

    /// Decide whether a request to `url` may leave the host, and say where it
    /// may go.
    ///
    /// # Errors
    /// [`WireError::InvalidUrl`] if the URL has no host this can reason about,
    /// and [`WireError::ConnectionFailed`] when the destination is private and
    /// unallowed — deliberately the same error a refused connection produces, so
    /// a component cannot use the gate as a port scanner that distinguishes
    /// "blocked" from "nothing listening".
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

        // An address literal needs no lookup — and must not get one, since a
        // resolver is exactly what an attacker would like us to consult.
        if let Ok(ip) = host.parse::<IpAddr>() {
            return if is_public(ip) {
                Ok(Destination::AsNamed)
            } else {
                Err(WireError::ConnectionFailed)
            };
        }

        // A name: every address it answers with has to be public. One private
        // answer is enough to refuse — a round-robin that sometimes points home
        // is not a destination this should reach on alternate attempts.
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
        // Every one of these was classified above, so connecting to any of them
        // is connecting to something this approved.
        Ok(Destination::Resolved(addrs))
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
        assert_eq!(
            policy.check("http://[::ffff:127.0.0.1]/"),
            Err(WireError::ConnectionFailed)
        );
    }

    #[test]
    fn a_configured_local_endpoint_is_reachable() {
        // The case this exists for: a self-hosted model on loopback.
        let policy = EgressPolicy::public_only().allowing("http://127.0.0.1:11434/v1");
        assert_eq!(
            policy.check("http://127.0.0.1:11434/v1/chat/completions"),
            Ok(Destination::AsNamed)
        );
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
        // The point of #108: what `check` approved is what the caller connects
        // to. `localhost` is the one name that resolves without a network, and
        // it resolves somewhere private — so the pair of assertions is that a
        // *name* takes the resolving path at all, and that the path refuses.
        let policy = EgressPolicy::public_only();
        assert_eq!(
            policy.check("http://localhost:8787/"),
            Err(WireError::ConnectionFailed)
        );

        // Granting the origin takes the other branch: nothing is resolved, so
        // there is nothing to pin and the client looks the name up itself.
        let policy = EgressPolicy::public_only().allowing("http://localhost:8787");
        assert_eq!(
            policy.check("http://localhost:8787/v1"),
            Ok(Destination::AsNamed)
        );
    }
}
