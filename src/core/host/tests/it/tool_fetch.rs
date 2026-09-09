//! `tool-fetch` across the Component-Model boundary.
//!
//! Drives the guest against a canned `host-http` so the whole path runs offline:
//! a public URL is fetched and reduced to text, and the SSRF guard refuses the
//! addresses that matter **without any request reaching the network at all** —
//! asserted by counting the calls the fake client received, not just by reading
//! the refusal message.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// A canned HTTP client that records how many requests it was actually asked to
/// make, so a test can prove a refusal happened *before* the network.
fn counting_http(calls: &Arc<AtomicU32>) -> HttpFn {
    let calls = Arc::clone(calls);
    Box::new(move |_m, _url, _h, _b, _t| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: b"<html><body><h1>Docs</h1><p>Hello &amp; welcome</p>\
                   <script>var secret='token'</script></body></html>"
                .to_vec(),
        })
    })
}

fn load(engine: &Engine, calls: &Arc<AtomicU32>) -> Option<ToolExtension> {
    if !common::guests_staged(&["tool-fetch.wasm"]) {
        return None;
    }
    let path = common::repo_root().join("ext").join("tool-fetch.wasm");
    let component = Component::from_file(engine, &path).expect("component compiles");
    let dir = std::env::temp_dir().join(format!("jk-toolfetch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();
    let workspace = Workspace::open(&dir).expect("workspace opens");
    Some(
        ToolExtension::instantiate_with_http(
            engine,
            "tool.fetch",
            &component,
            Some(workspace),
            ProcessRunner::disabled(),
            Some(counting_http(calls)),
        )
        .expect("tool instantiates"),
    )
}

#[test]
fn a_public_url_is_fetched_and_reduced_to_text() {
    let engine = Engine::default();
    let calls = Arc::new(AtomicU32::new(0));
    let Some(mut tool) = load(&engine, &calls) else {
        return;
    };

    let out = tool
        .invoke(r#"{"url":"https://doc.rust-lang.org/std/"}"#)
        .expect("fetch succeeds");
    assert!(
        out.contains("https://doc.rust-lang.org/std/ [200]"),
        "names its source: {out}"
    );
    assert!(
        out.contains("Docs Hello & welcome"),
        "markup stripped, entities decoded: {out}"
    );
    assert!(
        !out.contains("secret"),
        "script contents never reach the model: {out}"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "exactly one request was made"
    );
}

#[test]
fn blocked_addresses_never_reach_the_network() {
    let engine = Engine::default();
    let calls = Arc::new(AtomicU32::new(0));
    let Some(mut tool) = load(&engine, &calls) else {
        return;
    };

    for blocked in [
        // The agent's own REST surface — the most reachable target on the box.
        r#"{"url":"http://127.0.0.1:8787/sessions"}"#,
        // Cloud instance metadata, i.e. credentials.
        r#"{"url":"http://169.254.169.254/latest/meta-data/"}"#,
        // The LAN.
        r#"{"url":"http://192.168.1.1/admin"}"#,
        // Not HTTP at all.
        r#"{"url":"file:///etc/passwd"}"#,
        // Loopback wearing a hostname.
        r#"{"url":"http://localhost:8787/"}"#,
        // Loopback wearing a credential prefix.
        r#"{"url":"http://example.com@127.0.0.1/"}"#,
        // Loopback wearing an alternate encoding.
        r#"{"url":"http://2130706433/"}"#,
    ] {
        let out = tool
            .invoke(blocked)
            .expect("a refusal is a result, not a trap");
        assert!(
            out.starts_with("REFUSED:"),
            "{blocked} must be refused: {out}"
        );
    }

    // The point of the assertion: not one of those became a request.
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "the guard runs before the client"
    );
}

#[test]
fn a_tool_without_granted_egress_cannot_reach_the_network_at_all() {
    let engine = Engine::default();
    if !common::guests_staged(&["tool-fetch.wasm"]) {
        return;
    }
    let path = common::repo_root().join("ext").join("tool-fetch.wasm");
    let component = Component::from_file(&engine, &path).expect("component compiles");

    // No client passed: the same shape `build_agent` uses for an instance whose
    // config does not say `network: true`. `tool-world` still *imports*
    // `host-http` — the point is that importing an interface is not the same as
    // being granted the capability behind it.
    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.fetch",
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");

    let out = tool
        .invoke(r#"{"url":"https://example.com/"}"#)
        .expect("a failure is a result");
    assert!(
        out.starts_with("FAILED:"),
        "no egress without a grant: {out}"
    );
}
