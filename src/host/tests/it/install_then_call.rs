//! Install in one turn, call it in the next — one session, no restart
//! (#214).
//!
//! This is the slice's gate, and it is the only test that exercises the
//! whole seam: `ext-install` lands a component and leaves its stem on the
//! session; the driver of turns reads that between turns, has the
//! `Runtime` adopt it, and rebuilds; the next turn dispatches it.
//!
//! The rebuild is the part that is easy to get wrong in a way tests miss.
//! A component can be dispatchable and still invisible, because the tool
//! advertisement is computed once and injected into every interceptor's
//! config at instantiation — so a fleet that knew about the new tool while
//! the selector did not would pass any test that called the tool by name.
//! The second turn here goes through `interceptor-tool-selector`, so the
//! advertisement has to have been rebuilt too.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;

const NEEDED: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-hello.wasm",
];

/// What the model does on each turn, and what came back.
struct Script {
    /// Tool-call arguments per turn, popped in order.
    calls: Mutex<Vec<String>>,
    /// Tool names called, in order.
    named: Mutex<Vec<String>>,
    /// Request bodies carrying a tool result.
    results: Mutex<Vec<String>>,
    /// Whether the advertisement offered `hello` on each request.
    advertised_hello: Mutex<Vec<bool>>,
    calls_seen: AtomicU32,
}

fn provider(script: &Arc<Script>) -> impl Fn() -> jan_klod_core::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jan_klod_core::route::HttpFn {
        let script = Arc::clone(&script);
        Box::new(move |_m, _u, _h, body, _t| {
            script.calls_seen.fetch_add(1, Ordering::Relaxed);
            let text = body.map(|b| String::from_utf8_lossy(b).into_owned());
            if let Some(body) = &text {
                script
                    .advertised_hello
                    .lock()
                    .expect("not poisoned")
                    .push(body.contains("\"tool-hello\""));
            }
            let after_tool = text.as_deref().is_some_and(|body| {
                serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        v.get("messages")?
                            .as_array()?
                            .last()?
                            .get("role")?
                            .as_str()
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("tool")
            });
            if after_tool {
                if let Some(body) = text {
                    script.results.lock().expect("not poisoned").push(body);
                }
                return Ok(jan_klod_core::http::WireResponse {
                    status: 200,
                    headers: vec![],
                    body: serde_json::to_vec(&serde_json::json!({"choices":[{"message":{
                        "role":"assistant","content":"done"},"finish_reason":"stop"}]}))
                    .unwrap(),
                });
            }
            let next = script.calls.lock().expect("not poisoned").pop();
            let response = next.map_or_else(
                || {
                    serde_json::json!({"choices":[{"message":{
                        "role":"assistant","content":"nothing to do"},"finish_reason":"stop"}]})
                },
                |spec| {
                    let (name, arguments) = spec.split_once('|').expect("name|arguments");
                    script
                        .named
                        .lock()
                        .expect("not poisoned")
                        .push(name.to_owned());
                    serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                        {"id":"c1","function":{"name":name,"arguments":arguments}}]},
                        "finish_reason":"tool_calls"}]})
                },
            );
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    }
}

/// A runtime whose `ext/` holds the provider and the selector, with a
/// signed `tool-hello` waiting *outside* it. Returns the temp root, the
/// config and the offer.
fn fixture() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("jk-install-call-{}", std::process::id()));
    let ext = dir.join("ext");
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&ext).expect("creates the ext dir");
    std::fs::create_dir_all(&incoming).expect("creates the incoming dir");
    let staged = common::repo_root().join("ext");

    // The runtime starts *without* the tool: that is the whole point.
    for guest in ["provider-openai", "interceptor-tool-selector"] {
        for suffix in [".wasm", ".manifest.toml"] {
            let name = format!("{guest}{suffix}");
            if staged.join(&name).exists() {
                std::fs::copy(staged.join(&name), ext.join(&name)).expect("copies a guest");
            }
        }
    }
    // And the offer waits outside it, signed.
    let signer = common::minisig::Signer::new();
    for suffix in [".wasm", ".manifest.toml"] {
        let name = format!("tool-hello{suffix}");
        std::fs::copy(staged.join(&name), incoming.join(&name)).expect("copies the offer");
        signer.sign(&incoming.join(&name));
    }

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {db}
registry:
  install-tool: true
  trusted-keys:
    - {key}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    tool-selector:
      enabled: true
",
            key = signer.public_key_base64(),
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");

    (dir, config, incoming.join("tool-hello.wasm"))
}

#[test]
fn a_tool_installed_in_one_turn_answers_in_the_next() {
    if !common::guests_staged(&NEEDED) {
        return;
    }
    let (dir, config, source) = fixture();
    let _guard = common::TempDir(dir.clone());
    let ext = dir.join("ext");

    let script = Arc::new(Script {
        calls: Mutex::new(vec![
            // Popped in order: turn two calls the tool that turn one installs.
            "tool-hello|{}".to_string(),
            format!(
                "ext-install|{}",
                serde_json::json!({ "path": source.display().to_string() })
            ),
        ]),
        named: Mutex::new(Vec::new()),
        results: Mutex::new(Vec::new()),
        advertised_hello: Mutex::new(Vec::new()),
        calls_seen: AtomicU32::new(0),
    });

    let mut runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    let factory = provider(&script);
    let mut agent = runtime.build_agent(&factory).expect("the agent boots");

    // Turn one: install.
    agent.run("s1", "install the hello tool");

    // Between turns — the seam. This is what a host surface does.
    let installed = agent.take_installed();
    assert_eq!(
        installed,
        vec!["tool-hello".to_string()],
        "the install left no note, so nothing downstream can know to load it"
    );
    let mut outcomes = Vec::new();
    for stem in &installed {
        let outcome = runtime.adopt_installed(stem);
        // The load is recorded on the session: the install already said
        // "callable from the next turn", and that promise can fail.
        match &outcome {
            Ok(id) => agent.record_load("s1", stem, Ok(id.as_str())),
            Err(err) => agent.record_load("s1", stem, Err(&err.to_string())),
        }
        outcomes.push(outcome);
    }
    assert!(
        outcomes.iter().all(Result::is_ok),
        "adoption failed: {outcomes:?}"
    );
    drop(agent);
    let mut agent = runtime.build_agent(&factory).expect("the agent rebuilds");

    // Turn two: call it. Same session id, same process, no restart.
    agent.run("s1", "now greet the world");

    let named = script.named.lock().expect("not poisoned").clone();
    assert_eq!(
        named,
        vec!["ext-install".to_string(), "tool-hello".to_string()],
        "the two turns did not call what the script said"
    );

    let results = script.results.lock().expect("not poisoned").clone();
    assert_eq!(results.len(), 2, "both turns ran a tool");
    // `tool-hello` is the `ext-new` template guest, so its own answer is
    // the stub's "replace me". That it is *the guest's* string and not the
    // host's is the point: a refusal or an absent tool reads differently.
    assert!(
        results[1].contains("replace me"),
        "the freshly installed tool did not answer in the same session: {}",
        results[1]
    );

    // The load is in the session log, as a record rather than a turn event.
    // Without it the log ends on the install's "callable from the next
    // turn" — a promise, with nothing saying whether it held.
    let store =
        jan_klod_core::store::Store::open(dir.join("jan-klod.db")).expect("the log is readable");
    let rows = store.session_events("s1").expect("the session has a log");
    assert!(
        rows.iter().any(|row| {
            row.kind == jan_klod_core::event_log::KIND_EXTENSION_LOADED
                && row.payload.contains("tool-hello")
                && row.payload.contains("\"loaded\":true")
        }),
        "the adoption is not in the session log, so the log promises a tool \
         that nothing confirms arrived"
    );

    // And it was *offered*, not merely dispatchable. A fleet that knew the
    // tool while the selector did not would still pass the assertion above,
    // because the script calls it by name regardless of the advertisement.
    let advertised = script
        .advertised_hello
        .lock()
        .expect("not poisoned")
        .clone();
    assert_eq!(
        advertised.first(),
        Some(&false),
        "the tool was advertised before it was installed: {advertised:?}"
    );
    assert_eq!(
        advertised.last(),
        Some(&true),
        "the tool was installed and adopted but never advertised, so a real \
         model would never have known to call it: {advertised:?}"
    );
}
