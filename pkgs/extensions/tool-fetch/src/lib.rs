//! `tool-fetch` — retrieve a URL through `host-http` as readable text.
//!
//! `{ "url": "https://example.com/docs" }` → stripped page text.
//!
//! ## The address is the attack surface
//!
//! Unlike other tools (bounded by `host-fs`, `host-process`), `host-http` is unrestricted
//! outbound access to model-supplied destinations (textbook SSRF). Reachable targets:
//! cloud metadata (`169.254.169.254`), jan-klod REST (`127.0.0.1:8787`), LAN, `file://`.
//!
//! [`vet`] runs before every request: https/http only, no loopback/link-local/private
//! (unless `allow-private` opted in). Obfuscated addresses refused, not normalized.
//! In-sandbox guard stops confused models, not hostile components. Real boundary is
//! host-side `core::egress`, which resolves hostnames before deciding.
//!
//! ## Why disabled by default
//!
//! Fetch is a read, but URL is an exfiltration channel (path/query carry workspace data).
//! `tool.fetch` is `enabled: false`; enabling is a deployment decision.
//!
//! Vetting and extraction are pure Rust (unit-tested); glue only for wasm32.

/// Request timeout in milliseconds; a page that will not answer is not an answer.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const TIMEOUT_MS: u32 = 15_000;

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod fetch {
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Host names that always mean "this machine", whatever they resolve to.
    const LOCAL_NAMES: [&str; 4] = ["localhost", "ip6-localhost", "ip6-loopback", "metadata"];
    /// Suffixes that name a private or link-local naming scope.
    const LOCAL_SUFFIXES: [&str; 4] = [".localhost", ".local", ".internal", ".home.arpa"];

    /// Vet a model-supplied URL before making a request.
    /// # Errors
    /// Caller-facing refusal (model can correct and retry).
    pub fn vet(url: &str, allow_private: bool) -> Result<(), String> {
        let lowered = url.trim().to_lowercase();
        let rest = if let Some(rest) = lowered.strip_prefix("https://") {
            rest
        } else if let Some(rest) = lowered.strip_prefix("http://") {
            rest
        } else {
            return Err(format!(
                "only http:// and https:// URLs can be fetched (got `{}`)",
                scheme_of(&lowered)
            ));
        };

        // Authority ends at first `/`, `?`, or `#`.
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // `user@host`: host follows the last `@` (disguises `http://example.com@127.0.0.1/`).
        let hostport = authority.rsplit('@').next().unwrap_or("");
        let host = strip_port(hostport);
        if host.is_empty() {
            return Err("the URL has no host".to_string());
        }
        if allow_private {
            return Ok(());
        }
        if is_local_name(host) {
            return Err(format!("`{host}` names this machine or a private scope"));
        }
        match classify_address(host) {
            Address::Public | Address::PublicName => Ok(()),
            Address::NonPublic => Err(format!(
                "`{host}` is not a public address; set `allow-private` to permit it"
            )),
            Address::Unrecognised => Err(format!(
                "`{host}` is not a plain hostname or dotted-quad address — refusing an \
                 address form this tool cannot check"
            )),
        }
    }

    /// What an authority's host component turned out to be.
    #[derive(Debug, PartialEq, Eq)]
    pub enum Address {
        /// A dotted-quad or bracketed v6 literal in public space.
        Public,
        /// An ordinary hostname (its resolved address is not checked).
        PublicName,
        /// Loopback, link-local, private, unique-local, or unspecified.
        NonPublic,
        /// An encoding this guard will not try to canonicalise.
        Unrecognised,
    }

    /// Classify the host part of an authority.
    #[must_use]
    pub fn classify_address(host: &str) -> Address {
        if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            return classify_v6(v6);
        }
        // Anything made only of digits and dots is meant to be an address. If it
        // is not a clean dotted quad it is an alternate encoding (integer,
        // octal, short form) — refuse rather than guess.
        if host.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return classify_v4(host);
        }
        if host
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '.'))
        {
            return Address::Unrecognised;
        }
        // RFC 1123 forbids an all-numeric last label; resolvers read one as an
        // address anyway — how `0x7f.0.0.1` reaches loopback while looking like
        // a hostname. Treat it as an address so the dotted-quad rules catch it.
        if host
            .rsplit('.')
            .next()
            .is_some_and(|last| last.starts_with(|c: char| c.is_ascii_digit()))
        {
            return classify_v4(host);
        }
        Address::PublicName
    }

    /// Classify a v4 literal.
    ///
    /// Parsing is `Ipv4Addr::from_str`'s, not this file's, and that is the
    /// security-relevant part: it refuses every alternate encoding a resolver
    /// might read differently — `0177.0.0.1` (octal), `0x7f.0.0.1` (hex),
    /// `2130706433` (integer), `127.1` (short form), `010.0.0.1` (leading
    /// zero). This code used to refuse them by hand, which meant the refusal
    /// was only as good as the list somebody remembered to write.
    fn classify_v4(host: &str) -> Address {
        let Ok(addr) = host.parse::<Ipv4Addr>() else {
            return Address::Unrecognised;
        };
        if is_private_v4(addr) {
            Address::NonPublic
        } else {
            Address::Public
        }
    }

    /// Whether a v4 address is outside public routing.
    ///
    /// Six of these are the standard library's own predicates, kept current
    /// by whoever maintains it rather than by this file. The two below them
    /// have no stable predicate — `is_shared` and the rest of the `is_global`
    /// family are unstable — so they stay written out, which is a much
    /// smaller thing to keep right than the eight ranges that were here.
    #[must_use]
    pub fn is_private_v4(addr: Ipv4Addr) -> bool {
        addr.is_unspecified()      // 0.0.0.0/8 "this network"
            || addr.is_loopback()
            || addr.is_private()   // 10/8, 172.16/12, 192.168/16
            || addr.is_link_local() // 169.254/16 — cloud metadata
            || addr.is_multicast()
            || addr.is_broadcast()
            // CGNAT, 100.64/10: `is_shared` is unstable.
            || (addr.octets()[0] == 100 && (64..=127).contains(&addr.octets()[1]))
            // 240/4 reserved, which `is_multicast` (224/4) stops short of.
            || addr.octets()[0] >= 240
    }

    /// Classify a v6 literal.
    ///
    /// Parsed rather than prefix-matched. The previous version tested the
    /// text for `fe8`/`fe9`/`fea`/`feb`/`fc`/`fd`, which a compressed or
    /// zero-padded form slips past — `fe80:0000::1` starts with `fe80`, but
    /// so does nothing else it was written to catch, and `0:0:0:0:0:0:0:1` is
    /// loopback while starting with none of them.
    fn classify_v6(addr: &str) -> Address {
        let addr = addr.split('%').next().unwrap_or(addr); // drop a zone id
        let Ok(parsed) = addr.parse::<Ipv6Addr>() else {
            return Address::Unrecognised;
        };
        // IPv4-mapped (`::ffff:127.0.0.1`) is a v4 address wearing a v6 hat,
        // and must be judged by v4's rules or `::ffff:127.0.0.1` reaches
        // loopback through a guard that only looked at v6 ranges.
        if let Some(v4) = parsed.to_ipv4_mapped() {
            return if is_private_v4(v4) {
                Address::NonPublic
            } else {
                Address::Public
            };
        }
        if is_private_v6(parsed) {
            Address::NonPublic
        } else {
            Address::Public
        }
    }

    /// Whether a v6 address is outside public routing.
    ///
    /// Unique-local and link-local have no stable predicate either
    /// (`is_unique_local`, `is_unicast_link_local`), so they are written out
    /// — but over the parsed segments rather than the text, which is what
    /// makes a compressed form impossible to slip through.
    #[must_use]
    pub const fn is_private_v6(addr: Ipv6Addr) -> bool {
        let first = addr.segments()[0];
        addr.is_loopback()
            || addr.is_unspecified()
            || addr.is_multicast()
            || (first & 0xfe00) == 0xfc00 // fc00::/7 unique-local
            || (first & 0xffc0) == 0xfe80 // fe80::/10 link-local
    }

    /// Whether the host is a name that means this machine or a private scope.
    fn is_local_name(host: &str) -> bool {
        LOCAL_NAMES.contains(&host) || LOCAL_SUFFIXES.iter().any(|s| host.ends_with(s))
    }

    /// Drop a `:port` suffix, leaving bracketed v6 literals intact.
    fn strip_port(hostport: &str) -> &str {
        if hostport.starts_with('[') {
            return hostport.split(']').next().map_or(hostport, |h| {
                // Re-attach the bracket so the v6 branch still matches.
                &hostport[..=h.len()]
            });
        }
        hostport.split(':').next().unwrap_or(hostport)
    }

    /// The scheme of a URL, for a refusal message.
    fn scheme_of(url: &str) -> &str {
        url.split_once(':').map_or(url, |(scheme, _)| scheme)
    }

    /// Reduce an HTML document to readable text.
    ///
    /// A page is mostly markup; handing the model raw HTML spends the context
    /// budget on angle brackets. Script and style *contents* are dropped
    /// entirely (they are code, not prose), remaining tags are removed, the
    /// handful of entities that survive that are decoded, and whitespace runs
    /// collapse. Non-HTML bodies pass through untouched.
    #[must_use]
    pub fn to_text(body: &str) -> String {
        let stripped = drop_elements(body, &["script", "style"]);
        let mut text = String::with_capacity(stripped.len());
        let mut in_tag = false;
        for ch in stripped.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => {
                    in_tag = false;
                    text.push(' ');
                }
                _ if !in_tag => text.push(ch),
                _ => {}
            }
        }
        collapse(&decode_entities(&text))
    }

    /// Remove `<name …> … </name>` spans, contents included.
    fn drop_elements(body: &str, names: &[&str]) -> String {
        let mut out = body.to_string();
        for name in names {
            let open = format!("<{name}");
            let close = format!("</{name}>");
            loop {
                let lowered = out.to_lowercase();
                let Some(start) = lowered.find(&open) else {
                    break;
                };
                let end = lowered[start..]
                    .find(&close)
                    .map_or(out.len(), |offset| start + offset + close.len());
                out.replace_range(start..end, " ");
            }
        }
        out
    }

    /// Decode the entities that actually show up in prose.
    fn decode_entities(text: &str) -> String {
        text.replace("&nbsp;", " ")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&amp;", "&")
    }

    /// Collapse whitespace runs, keeping paragraph breaks readable.
    fn collapse(text: &str) -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }

    /// Format a fetched page for the model.
    #[must_use]
    pub fn render(url: &str, status: u16, body: &str) -> String {
        let text = to_text(body);
        let text = if text.is_empty() {
            "(the page had no readable text)"
        } else {
            &text
        };
        guest_fs::truncate(format!("{url} [{status}]\n\n{text}"))
    }

    #[cfg(test)]
    mod tests {
        use super::{classify_address, is_private_v4, render, to_text, vet, Address};

        #[test]
        fn only_http_and_https_are_fetchable() {
            assert!(vet("https://example.com/a", false).is_ok());
            assert!(vet("http://example.com", false).is_ok());
            for bad in [
                "file:///etc/passwd",
                "ftp://example.com",
                "gopher://example.com",
                "data:text/html,<script>x</script>",
                "javascript:alert(1)",
                "example.com",
            ] {
                let err = vet(bad, false).unwrap_err();
                assert!(err.contains("only http:// and https://"), "{bad}: {err}");
            }
        }

        #[test]
        fn loopback_and_the_agents_own_surface_are_refused() {
            // jan-klod's REST surface is the most interesting target on the box.
            for bad in [
                "http://127.0.0.1:8787/sessions",
                "http://localhost:8787/",
                "https://[::1]/",
                "http://127.1.2.3/",
            ] {
                assert!(vet(bad, false).is_err(), "{bad} must be refused");
            }
        }

        #[test]
        fn cloud_metadata_is_refused() {
            // The single most valuable SSRF target: instance credentials.
            assert!(vet("http://169.254.169.254/latest/meta-data/", false).is_err());
            assert!(vet("http://metadata/computeMetadata/v1/", false).is_err());
            assert!(vet("http://metadata.google.internal/", false).is_err());
        }

        #[test]
        fn private_ranges_are_refused() {
            for bad in [
                "http://10.0.0.5/",
                "http://192.168.1.1/",
                "http://172.16.0.1/",
                "http://172.31.255.254/",
                "https://[fd00::1]/",
                "https://[fe80::1]/",
            ] {
                assert!(vet(bad, false).is_err(), "{bad} must be refused");
            }
            // 172.32 is public — the /12 boundary must not over-block.
            assert!(vet("http://172.32.0.1/", false).is_ok());
        }

        #[test]
        fn credentials_in_the_url_cannot_disguise_the_host() {
            // The real destination is after the LAST `@`.
            assert!(vet("http://example.com@127.0.0.1/", false).is_err());
            assert!(vet("http://a@b@169.254.169.254/", false).is_err());
        }

        #[test]
        fn obfuscated_address_forms_are_refused_not_normalised() {
            for bad in [
                "http://2130706433/", // integer form of 127.0.0.1
                "http://0177.0.0.1/", // octal octet
                "http://127.1/",      // short form
                "http://0x7f.0.0.1/", // hex octet
            ] {
                let err = vet(bad, false).unwrap_err();
                assert!(
                    err.contains("address form") || err.contains("not a public address"),
                    "{bad}: {err}"
                );
            }
        }

        #[test]
        fn allow_private_opens_the_intranet_but_never_the_scheme() {
            assert!(vet("http://192.168.1.10/wiki", true).is_ok());
            assert!(vet("http://localhost:8080/", true).is_ok());
            // Scheme is not a privacy question: still refused.
            assert!(vet("file:///etc/passwd", true).is_err());
        }

        #[test]
        fn ordinary_public_urls_pass() {
            for good in [
                "https://doc.rust-lang.org/std/",
                "https://example.com:8443/path?q=1#frag",
                "http://93.184.216.34/",
            ] {
                assert!(vet(good, false).is_ok(), "{good} should be allowed");
            }
        }

        #[test]
        fn address_classification_is_explicit() {
            assert_eq!(classify_address("example.com"), Address::PublicName);
            assert_eq!(classify_address("8.8.8.8"), Address::Public);
            assert_eq!(classify_address("10.1.2.3"), Address::NonPublic);
            assert_eq!(classify_address("999.1.1.1"), Address::Unrecognised);
            assert!(is_private_v4(std::net::Ipv4Addr::new(169, 254, 169, 254)));
            assert!(!is_private_v4(std::net::Ipv4Addr::new(1, 1, 1, 1)));
        }

        /// The encodings a resolver may read differently from this guard.
        /// Each one reaches loopback if it is canonicalised rather than
        /// refused, which is why `Ipv4Addr::from_str` doing the refusing
        /// matters more than the range checks after it.
        #[test]
        fn every_alternate_encoding_of_loopback_is_refused() {
            for encoded in [
                "0177.0.0.1", // octal
                "0x7f.0.0.1", // hex
                "2130706433", // integer
                "127.1",      // short form
                "010.0.0.1",  // leading zero
                "127.0.0.01", // leading zero, last octet
            ] {
                assert_eq!(
                    classify_address(encoded),
                    Address::Unrecognised,
                    "{encoded} was not refused"
                );
            }
            // The canonical spelling still classifies, so the refusal above
            // is about the encoding and not about the address.
            assert_eq!(classify_address("127.0.0.1"), Address::NonPublic);
        }

        /// A v6 form that a prefix match over the text would miss.
        #[test]
        fn a_compressed_or_padded_v6_is_classified_by_value_not_by_spelling() {
            for spelled in [
                "[::1]",                    // loopback, compressed
                "[0:0:0:0:0:0:0:1]",        // loopback, written out
                "[fe80::1]",                // link-local
                "[fe80:0000:0000:0000::1]", // link-local, padded
                "[febf::1]",                // link-local, top of the range
                "[fc00::1]",                // unique-local
                "[fd12:3456::1]",           // unique-local
                "[::ffff:127.0.0.1]",       // v4-mapped loopback
            ] {
                assert_eq!(
                    classify_address(spelled),
                    Address::NonPublic,
                    "{spelled} was not recognised as non-public"
                );
            }
            assert_eq!(classify_address("[2606:4700::1111]"), Address::Public);
            assert_eq!(classify_address("[not:an:address]"), Address::Unrecognised);
        }

        /// CGNAT and the reserved top of the v4 space have no stable
        /// predicate, so they are the two this file still spells out.
        #[test]
        fn the_ranges_stdlib_has_no_predicate_for_are_still_covered() {
            assert_eq!(classify_address("100.64.0.1"), Address::NonPublic);
            assert_eq!(classify_address("100.127.255.255"), Address::NonPublic);
            assert_eq!(classify_address("240.0.0.1"), Address::NonPublic);
            // Just outside CGNAT, so public.
            assert_eq!(classify_address("100.128.0.1"), Address::Public);
            assert_eq!(classify_address("100.63.255.255"), Address::Public);
        }

        #[test]
        fn script_and_style_contents_never_reach_the_model() {
            let css = format!("body{}color:red{}", '{', '}');
            let style = format!("<style>{css}</style>");
            let html = format!(
                "<html><head>{style}<script>var token='secret'</script></head>\
                 <body><h1>Title</h1><p>Hello &amp; welcome</p></body></html>"
            );
            let text = to_text(&html);
            assert!(!text.contains("color:red"), "style dropped: {text}");
            assert!(!text.contains("secret"), "script dropped: {text}");
            assert_eq!(text, "Title Hello & welcome");
        }

        #[test]
        fn entities_and_whitespace_are_normalised() {
            assert_eq!(to_text("<p>a &lt;b&gt;   c</p>\n\n  d"), "a <b> c d");
            assert_eq!(to_text("plain text"), "plain text");
        }

        #[test]
        fn an_empty_page_says_so_rather_than_returning_a_bare_status() {
            let out = render("https://x.test/", 204, "<html><body></body></html>");
            assert!(out.contains("204"), "{out}");
            assert!(out.contains("no readable text"), "{out}");
        }

        #[test]
        fn the_result_names_the_url_and_status_it_came_from() {
            let out = render("https://x.test/a", 200, "<p>hi</p>");
            assert!(out.starts_with("https://x.test/a [200]"), "{out}");
            assert!(out.ends_with("hi"), "{out}");
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::{fetch, TIMEOUT_MS};
    use core::cell::Cell;

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod bindings {
        wit_bindgen::generate!({ world: "tool-world", path: "../../../wit" });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_http;

    thread_local! {
        /// `allow-private`, read once at `init`. Default **false**: a deployment
        /// opts into its own intranet, it is never assumed.
        static ALLOW_PRIVATE: Cell<bool> = const { Cell::new(false) };
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
            let allow = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|section| {
                    section
                        .get("allow-private")
                        .and_then(serde_json::Value::as_bool)
                })
                .unwrap_or(false);
            ALLOW_PRIVATE.with(|flag| flag.set(allow));
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

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "fetch".to_string(),
                description: "Fetch a public http(s) URL and return its readable text. \
                    Private, loopback, and link-local addresses are refused."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "an http:// or https:// URL" }
                    },
                    "required": ["url"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let url = value
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;

            // A refused address is a recoverable answer the model can act on —
            // it may know a public URL for the same thing.
            let allow_private = ALLOW_PRIVATE.with(Cell::get);
            if let Err(reason) = fetch::vet(url, allow_private) {
                return Ok(format!("REFUSED: {reason}"));
            }

            let request = host_http::HttpRequest {
                method: "GET".to_string(),
                url: url.to_string(),
                headers: vec![host_http::HttpHeader {
                    name: "Accept".to_string(),
                    value: "text/html,text/plain;q=0.9,*/*;q=0.8".to_string(),
                }],
                body: None,
                timeout_ms: TIMEOUT_MS,
            };
            match host_http::fetch(&request) {
                Ok(response) => {
                    let body = String::from_utf8_lossy(&response.body);
                    Ok(fetch::render(url, response.status, &body))
                }
                // A transport failure is information too: the model may retry a
                // different source rather than abandoning the task.
                Err(err) => Ok(format!("FAILED: {url} could not be fetched ({err:?})")),
            }
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
