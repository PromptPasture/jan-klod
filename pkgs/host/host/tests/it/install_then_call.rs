//! Install in one turn, call it in the next, in one session with no restart (#214). The only test of the whole seam:
//! the tool advertisement is injected at instantiation, so a rebuild must refresh the selector too.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jk_agent::Runtime;

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

fn provider(script: &Arc<Script>) -> impl Fn() -> jk_wasm::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jk_wasm::route::HttpFn {
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
                return Ok(jk_wasm::http::WireResponse {
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
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    }
}

/// A runtime whose `ext/` holds the provider and selector, with a signed
/// `tool-hello` waiting outside it. Returns the temp root, config and offer.
fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    // Keyed by test: parallel tests sharing one `ext/` would install into each other's runtime.
    let dir = std::env::temp_dir().join(format!("jk-install-call-{tag}-{}", std::process::id()));
    let ext = dir.join("ext");
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&ext).expect("creates the ext dir");
    std::fs::create_dir_all(&incoming).expect("creates the incoming dir");
    let staged = common::repo_root().join("ext");

    for guest in ["provider-openai", "interceptor-tool-selector"] {
        for suffix in [".wasm", ".manifest.toml"] {
            let name = format!("{guest}{suffix}");
            if staged.join(&name).exists() {
                std::fs::copy(staged.join(&name), ext.join(&name)).expect("copies a guest");
            }
        }
    }
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
    let (dir, config, source) = fixture("one-session");
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

    let runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    let factory = provider(&script);
    let agents = jk_host::sessions::Agents::new(runtime, Arc::new(factory));

    // The adoption runs in the session thread between turns (#231), for every transport.
    let one = agents.of("s1");
    one.run(|agent: &mut jk_agent::AgentSession| {
        agent.run("s1", "install the hello tool");
    })
    .expect("turn one ran");

    // Same session; the adoption rebuilt the agent underneath.
    agents
        .of("s1")
        .run(|agent: &mut jk_agent::AgentSession| {
            agent.run("s1", "now greet the world");
        })
        .expect("turn two ran");

    let named = script.named.lock().expect("not poisoned").clone();
    assert_eq!(
        named,
        vec!["ext-install".to_string(), "tool-hello".to_string()],
        "the two turns did not call what the script said"
    );

    let results = script.results.lock().expect("not poisoned").clone();
    assert_eq!(results.len(), 2, "both turns ran a tool");
    // "replace me" is the ext-new stub's answer; a host refusal or absent tool reads differently.
    assert!(
        results[1].contains("replace me"),
        "the freshly installed tool did not answer in the same session: {}",
        results[1]
    );

    // The load must be recorded in the session log, not just promised by the install.
    let store =
        jk_session::store::Store::open(dir.join("jan-klod.db")).expect("the log is readable");
    let rows = store.session_events("s1").expect("the session has a log");
    assert!(
        rows.iter().any(|row| {
            row.kind == jk_session::event_log::KIND_EXTENSION_LOADED
                && row.payload.contains("tool-hello")
                && row.payload.contains("\"loaded\":true")
        }),
        "the adoption is not in the session log, so the log promises a tool \
         that nothing confirms arrived"
    );

    // Offered, not just dispatchable: the script calls by name regardless of the advertisement.
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

/// A session that installed nothing can call a tool another session installed (#231).
///
/// Any "only the installing session" implementation gets this wrong. Driven through
/// the registry, so the adoption is what a surface does, not what the test does.
#[test]
fn a_session_that_installed_nothing_can_call_what_another_installed() {
    if !common::guests_staged(&NEEDED) {
        return;
    }
    let (dir, config, source) = fixture("bystander");
    let _guard = common::TempDir(dir.clone());
    let ext = dir.join("ext");

    let script = Arc::new(Script {
        calls: Mutex::new(vec![
            // Popped from the back: B calls the tool it never installed.
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

    let runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    let factory = provider(&script);
    let agents = jk_host::sessions::Agents::new(runtime, Arc::new(factory));

    // The adoption runs inside A's session thread, after its turn.
    agents
        .of("installer")
        .run(|agent: &mut jk_agent::AgentSession| {
            agent.run("installer", "install the hello tool");
        })
        .expect("A's turn ran");

    agents
        .of("bystander")
        .run(|agent: &mut jk_agent::AgentSession| {
            agent.run("bystander", "now greet the world");
        })
        .expect("B's turn ran");

    let named = script.named.lock().expect("not poisoned").clone();
    assert_eq!(
        named,
        vec!["ext-install".to_string(), "tool-hello".to_string()],
        "the two sessions did not call what the script said"
    );
    let results = script.results.lock().expect("not poisoned").clone();
    let last = results.last().cloned().unwrap_or_default();
    assert!(
        !last.contains("no tool named"),
        "the bystander could not see what another session installed: {last}"
    );
    // The advertisement must be rebuilt for B too: an unoffered tool is unusable by a model.
    let advertised = script
        .advertised_hello
        .lock()
        .expect("not poisoned")
        .clone();
    assert!(
        advertised.last() == Some(&true),
        "the new tool was dispatchable but never advertised to the bystander: {advertised:?}"
    );
}
