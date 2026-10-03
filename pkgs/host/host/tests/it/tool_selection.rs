//! What the tool selector costs and saves, against a real fleet (#219).
//!
//! `interceptor_host.rs` drives the same component against tools written in
//! the test. These drive it against the tools `self-extend` actually ships,
//! read out of a booted runtime rather than restated here — the whole claim
//! of the slice is about prompt bytes, and bytes invented in a test fixture
//! would measure the fixture.
//!
//! # The distribution is read, not copied
//!
//! The config is the shipped `self-extend/config.yaml` with three
//! substitutions: a mock provider URL, a literal key in place of the
//! environment one, and temporary paths. Each is asserted to have matched,
//! so a reordered or rewritten distribution fails here instead of quietly
//! measuring something nobody ships.

use jan_klod_core::intercept::{Dispatcher, HookState, Message, PendingRequest, Phase, Role};
use jan_klod_core::interceptor_host::WasmInterceptor;
use jan_klod_core::ConfigSection;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;
use crate::self_extend::shipped_fleet;

/// A driver that is never asked: `select-tools` has no question to put.
struct NoDriver;
impl jan_klod_core::intercept::Driver for NoDriver {
    fn ask(&mut self, _prompt: &jan_klod_core::intercept::UserPrompt) -> String {
        unreachable!("select-tools does not ask")
    }
}

/// The selector, configured with `tools` and whatever else the case needs.
fn selector(tools: &serde_json::Value, keys: &[(&str, usize)]) -> WasmInterceptor {
    let mut config = serde_json::json!({ "tools": tools.clone() });
    for (key, value) in keys {
        config[*key] = serde_json::json!(value);
    }
    let engine = Engine::default();
    let path = common::repo_root()
        .join("ext")
        .join("interceptor-tool-selector.wasm");
    let component = Component::from_file(&engine, &path).expect("the selector compiles");
    WasmInterceptor::instantiate(
        &engine,
        "interceptor.tool-selector",
        &component,
        ConfigSection::new(config),
        Box::new(|_| String::new()),
    )
    .expect("the selector instantiates")
}

/// What the selector advertises for `asked`.
fn advertised(
    tools: &serde_json::Value,
    keys: &[(&str, usize)],
    asked: &str,
) -> Vec<(String, String, String)> {
    let mut dispatcher = Dispatcher::new(vec![Box::new(selector(tools, keys))]);
    let mut state = HookState::SelectTools(PendingRequest {
        model: Some("m".into()),
        messages: vec![Message {
            role: Role::User,
            content: asked.to_string(),
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: None,
        temperature: None,
    });
    dispatcher.dispatch(Phase::SelectTools, &mut state, &mut NoDriver);
    let HookState::SelectTools(request) = state else {
        panic!("the state case changed")
    };
    request
        .tools
        .into_iter()
        .map(|tool| (tool.name, tool.description, tool.parameters_schema))
        .collect()
}

/// Bytes a tool costs in the prompt: its name, its prose, and its schema.
///
/// Every provider wraps these differently, so the wrapper is left out and
/// what is compared is the payload each of them carries.
fn schema_bytes(tools: &[(String, String, String)]) -> usize {
    tools
        .iter()
        .map(|(name, description, schema)| name.len() + description.len() + schema.len())
        .sum()
}

/// The acceptance, against the shipped fleet: a relevant tool is advertised
/// and an unrelated one is not.
#[test]
fn a_bounded_fleet_keeps_the_relevant_tool_and_drops_the_unrelated_one() {
    let Some((_guard, tools)) = shipped_fleet() else {
        return;
    };
    let kept = advertised(
        &tools,
        &[("max-tools", 3), ("min-tools", 1)],
        "show me the git diff of what I changed",
    );
    let names: Vec<&str> = kept.iter().map(|(name, ..)| name.as_str()).collect();
    assert!(names.contains(&"git"), "git was dropped: {names:?}");
    assert!(
        !names.contains(&"shell"),
        "running commands has nothing to do with reading a diff: {names:?}"
    );
}

/// Nothing matched is the case that decides whether this is safe: the floor
/// answers, rather than an empty tool list.
#[test]
fn a_request_matching_nothing_still_advertises_the_floor() {
    let Some((_guard, tools)) = shipped_fleet() else {
        return;
    };
    let kept = advertised(
        &tools,
        &[("max-tools", 3), ("min-tools", 2)],
        "xylophone marzipan",
    );
    assert_eq!(kept.len(), 2, "the floor did not hold: {kept:?}");
}

/// Configured with nothing, the selector advertises the fleet — so this
/// slice cannot have quietly hidden a tool from an existing deployment.
#[test]
fn an_unconfigured_selector_advertises_the_whole_fleet() {
    let Some((_guard, tools)) = shipped_fleet() else {
        return;
    };
    let whole = tools.as_array().expect("an array of tools").len();
    let kept = advertised(&tools, &[], "show me the git diff of what I changed");
    assert_eq!(kept.len(), whole, "the default is not a pass-through");
}

/// The measurement the slice exists for: what a bound saves in prompt bytes,
/// against the fleet `self-extend` ships.
///
/// Asserted as a direction and a floor on the saving rather than a fixed
/// number — the figure moves whenever a tool's schema does, and a test
/// pinning it would fail for reasons that have nothing to do with selection.
/// The figure itself is recorded on #219.
#[test]
fn the_bound_saves_prompt_bytes_and_says_how_many() {
    let Some((_guard, tools)) = shipped_fleet() else {
        return;
    };
    let asked = "find where the retry budget is set and edit it";
    let whole = advertised(&tools, &[], asked);
    let (count, bytes) = (whole.len(), schema_bytes(&whole));
    println!("self-extend, unbounded: {count} tools, {bytes} schema bytes");

    let mut last = bytes;
    for most in [4, 3, 2] {
        let bounded = advertised(&tools, &[("max-tools", most), ("min-tools", 2)], asked);
        let after = schema_bytes(&bounded);
        let saved = 100 - (after * 100 / bytes);
        println!(
            "  max-tools={most}: {} tools, {after} bytes ({saved}% saved)",
            bounded.len()
        );
        assert!(
            after < last,
            "a tighter bound advertised more: {after} >= {last}"
        );
        last = after;
    }
}
