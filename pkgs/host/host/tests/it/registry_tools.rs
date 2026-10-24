//! `ext-search` and `ext-install <name>` as the model reaches them, over a local index and a canned client.

use std::fmt::Write as _;

use jk_protocol::intercept::ToolInvoker;
use jk_protocol::turn::ToolCall;
use jk_wasm::native_tools::NativeTools;

use crate::common;
use crate::ext_install::{canned_files, published, scratch, Scratch};

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
            "api-version": jk_wasm::manifest::API_VERSION,
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
    assert_eq!(tools.take_installed(), [] as [std::string::String; 0]);
    assert!(!scratch.ext.join("tool-nothing.wasm").exists());
}
