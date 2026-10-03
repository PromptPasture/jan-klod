//! `ext-search` and `ext-install <name>` as the *model* reaches them (#218).
//!
//! `ext_registry.rs` covers the same registry through `ext_index`, which is
//! the operator's path (`jan-klod-gateway ext search` / `ext install`).
//! These cover the host-implemented tools over it: what a model is shown,
//! and what it is refused.
//!
//! No socket is opened. The index is a local path — which is not a
//! test-only shortcut but what an air-gapped operator with a mirrored index
//! uses — and the component is served by a canned client.

use std::fmt::Write as _;

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::intercept::ToolCall;
use jan_klod_core::native_tools::NativeTools;

use crate::common;
use crate::ext_install::{canned_files, published, scratch, Scratch};

/// An index describing `tool-fs` at a URL the canned client answers.
fn write_index(scratch: &Scratch, component: &[u8], signature: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(component);
    let hex = digest.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    let document = serde_json::json!({
        "index-version": 1,
        "extensions": [{
            "name": "tool-fs",
            "version": "0.1.0",
            "api-version": jan_klod_core::manifest::API_VERSION,
            "kind": "tool",
            "capabilities": ["host-fs", "host-log"],
            "description": "reads and writes inside the workspace",
            "author": "PromptPasture",
            "url": "https://example.com/ext/tool-fs.wasm",
            "sha256": hex,
            "signature": signature,
            "size": component.len(),
        }]
    });
    let path = scratch.incoming.join("index.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&document).expect("json"),
    )
    .expect("writes the fixture index");
    path.display().to_string()
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "c1".to_string(),
        name: name.to_string(),
        arguments: arguments.to_string(),
    }
}

/// A tools fixture over a signed `tool-fs` published to a canned server.
fn fixture(tag: &str) -> Option<(Scratch, NativeTools)> {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return None;
    }
    let scratch = scratch(tag);
    let (files, checks) = published(&scratch, "tool-fs");
    let component = files[0].1.clone();
    let signature = String::from_utf8(files[2].1.clone()).expect("a .minisig is text");
    let source = write_index(&scratch, &component, &signature);
    let (http, _) = canned_files(files);
    let tools =
        NativeTools::installing_into(scratch.ext.clone(), checks.trusted_keys, Some(source))
            .with_http(http);
    Some((scratch, tools))
}

/// What a model is shown: the capability line, and whether anything vouches
/// for the bytes.
#[test]
fn search_shows_capabilities_and_whether_it_is_signed() {
    let Some((_scratch, mut tools)) = fixture("search") else {
        return;
    };
    let out = tools
        .invoke(&call("ext-search", r#"{"term":"fs"}"#))
        .expect("ours");
    assert!(!out.failed, "{}", out.content);
    let body: serde_json::Value = serde_json::from_str(&out.content).expect("json");
    let first = &body["matches"][0];
    assert_eq!(first["name"], "tool-fs");
    assert_eq!(
        first["capabilities"],
        serde_json::json!(["host-fs", "host-log"]),
        "the capability line is the reason this format exists"
    );
    assert_eq!(
        first["signed"], true,
        "a model cannot tell a signed entry from an unsigned one: {first}"
    );
}

/// The acceptance: a name from the index installs, and lands for adoption.
#[test]
fn a_named_component_installs_and_is_queued_for_the_next_turn() {
    let Some((scratch, mut tools)) = fixture("install") else {
        return;
    };
    let out = tools
        .invoke(&call(
            "ext-install",
            r#"{"name":"tool-fs","reason":"the task needs to read files"}"#,
        ))
        .expect("ours");
    assert!(!out.failed, "{}", out.content);
    assert!(
        scratch.ext.join("tool-fs.wasm").exists(),
        "the component did not land"
    );
    assert_eq!(
        tools.take_installed(),
        vec!["tool-fs".to_string()],
        "the install left no note, so nothing downstream can adopt it (#214)"
    );
}

/// No reason, no install.
///
/// The prompt an operator answers is built from these arguments, so an
/// absent reason is silently no reason — and they would never know one was
/// expected. Enforced rather than encouraged for that reason.
#[test]
fn a_registry_install_without_a_reason_is_refused() {
    let Some((scratch, mut tools)) = fixture("reason") else {
        return;
    };
    let out = tools
        .invoke(&call("ext-install", r#"{"name":"tool-fs"}"#))
        .expect("ours");
    assert!(out.failed);
    assert!(
        out.content.contains("`reason` is required"),
        "{}",
        out.content
    );
    assert!(
        !scratch.ext.join("tool-fs.wasm").exists(),
        "a refused install still landed the component"
    );
}

/// A name the index does not list is refused, and says which index was
/// searched.
///
/// "Not found" from an index that turned out to be the wrong one is a
/// different problem from a name that does not exist, and the model cannot
/// see the difference unless the refusal says.
#[test]
fn a_name_the_index_does_not_list_is_refused() {
    let Some((scratch, mut tools)) = fixture("unknown") else {
        return;
    };
    let out = tools
        .invoke(&call(
            "ext-install",
            r#"{"name":"tool-nothing","reason":"because"}"#,
        ))
        .expect("ours");
    assert!(out.failed);
    assert!(
        out.content.contains("index.json"),
        "the refusal should name the index it searched: {}",
        out.content
    );
    assert!(tools.take_installed().is_empty());
    assert!(!scratch.ext.join("tool-nothing.wasm").exists());
}
