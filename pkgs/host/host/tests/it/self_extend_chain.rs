//! Compile, install, load, call in one session with no restart (#221); each link is tested alone elsewhere.
//! Runs in the shipped `self-extend` posture. Skips until `make self-extend-fixture` has prepared the crate.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jk_agent::Runtime;

use crate::common;
use crate::self_extend::shipped_execution_block;

/// The guests the chain needs staged before any of it can run.
const NEEDED: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-shell.wasm",
    "tool-fs.wasm",
];

/// The compiled tool's answer, which reaches the model only if every link ran.
const PHRASE: &str = "compiled-in-session";

/// Where turn 5's digest is spliced in.
const DIGEST: &str = "{{sha256}}";

/// The scripted model: queued `name|arguments` calls and what came back.
struct Script {
    calls: Mutex<std::collections::VecDeque<String>>,
    named: Mutex<Vec<String>>,
    results: Mutex<Vec<String>>,
    digest: Mutex<Option<String>>,
    completions: AtomicU32,
}

/// The first 64-hex-character run in `text`, which is what `shasum -a 256`
/// prints before the filename.
fn sha256_in(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|run| run.len() == 64)
        .map(str::to_lowercase)
}

fn provider(script: &Arc<Script>) -> impl Fn() -> jk_wasm::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jk_wasm::route::HttpFn {
        let script = Arc::clone(&script);
        Box::new(move |_m, _u, _h, body, _t| {
            script.completions.fetch_add(1, Ordering::Relaxed);
            let text = body
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let after_tool = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    Some(
                        v.get("messages")?
                            .as_array()?
                            .last()?
                            .get("role")?
                            .as_str()?
                            == "tool",
                    )
                })
                .unwrap_or(false);

            if after_tool {
                if let Some(found) = sha256_in(&text) {
                    *script.digest.lock().expect("not poisoned") = Some(found);
                }
                script.results.lock().expect("not poisoned").push(text);
                return Ok(done());
            }

            let next = script.calls.lock().expect("not poisoned").pop_front();
            let Some(spec) = next else {
                return Ok(done());
            };
            let (name, arguments) = spec.split_once('|').expect("name|arguments");
            let arguments = script
                .digest
                .lock()
                .expect("not poisoned")
                .as_deref()
                .map_or_else(
                    || arguments.to_owned(),
                    |digest| arguments.replace(DIGEST, digest),
                );
            script
                .named
                .lock()
                .expect("not poisoned")
                .push(name.to_owned());
            Ok(reply(
                &serde_json::json!({"choices":[{"message":{"role":"assistant",
                "tool_calls":[{"id":"c1","function":{"name":name,"arguments":arguments}}]},
                "finish_reason":"tool_calls"}]}),
            ))
        })
    }
}

fn done() -> jk_wasm::http::WireResponse {
    reply(
        &serde_json::json!({"choices":[{"message":{"role":"assistant",
        "content":"done"},"finish_reason":"stop"}]}),
    )
}

fn reply(value: &serde_json::Value) -> jk_wasm::http::WireResponse {
    jk_wasm::http::WireResponse {
        status: 200,
        headers: vec![],
        body: serde_json::to_vec(value).expect("serialises"),
    }
}

/// The prepared crate, or a loud skip naming how to get it.
fn fixture_crate() -> Option<std::path::PathBuf> {
    let fixture = common::repo_root().join(".fixtures/self-extend-guest");
    if fixture.join("Cargo.lock").exists() {
        return Some(fixture);
    }
    eprintln!(
        "skipping: .fixtures/self-extend-guest is not prepared — run \
         `make self-extend-fixture` once (it locks and fetches, online)"
    );
    None
}

/// A workspace holding the crate to compile, an `ext/` holding the fleet,
/// and a config in `self-extend`'s posture.
fn fixture() -> Option<(common::TempDir, std::path::PathBuf, std::path::PathBuf)> {
    let source = fixture_crate()?;
    let dir = std::env::temp_dir().join(format!("jk-chain-{}", std::process::id()));
    let guard = common::TempDir(dir.clone());
    let (ws, ext) = (dir.join("workspace"), dir.join("ext"));
    std::fs::create_dir_all(ws.join("src")).expect("creates the workspace");
    std::fs::create_dir_all(ws.join("wit")).expect("creates the wit dir");
    std::fs::create_dir_all(&ext).expect("creates the ext dir");

    for file in ["Cargo.toml", "Cargo.lock"] {
        std::fs::copy(source.join(file), ws.join(file)).expect("copies the crate");
    }
    std::fs::copy(source.join("src/lib.rs"), ws.join("src/lib.rs")).expect("copies the source");
    for entry in std::fs::read_dir(source.join("wit")).expect("reads the fixture's wit") {
        let entry = entry.expect("a wit file");
        std::fs::copy(entry.path(), ws.join("wit").join(entry.file_name())).expect("copies it");
    }

    let staged = common::repo_root().join("ext");
    for guest in [
        "provider-openai",
        "interceptor-tool-selector",
        "tool-shell",
        "tool-fs",
    ] {
        for suffix in [".wasm", ".manifest.toml"] {
            let name = format!("{guest}{suffix}");
            std::fs::copy(staged.join(&name), ext.join(&name)).expect("stages a guest");
        }
    }

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {db}
workspace: {ws}
registry:
  install-tool: true
  trusted-keys: []
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
  tool:
    shell:
      enabled: true
    fs:
      enabled: true
{execution}
",
            db = dir.join("jan-klod.db").display(),
            ws = ws.display(),
            execution = shipped_execution_block("self-extend"),
        ),
    )
    .expect("writes the config");
    Some((guard, config, ws))
}

/// The manifest the installer requires. `capabilities = []` is checked by the
/// host against the component's real imports.
fn manifest_toml() -> String {
    format!(
        "name = \"tool-selfbuilt\"\nversion = \"0.0.0\"\napi-version = \"{api}\"\n\
         kind = \"tool\"\ndescription = \"built in session\"\ncapabilities = []\n",
        api = jk_wasm::manifest::API_VERSION
    )
}

/// The six calls, in order, as the scripted model makes them.
fn chain_script(component: &std::path::Path) -> Arc<Script> {
    Arc::new(Script {
        calls: Mutex::new(std::collections::VecDeque::from(vec![
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "cargo",
                    "args": ["build", "--offline", "--locked", "--target", "wasm32-wasip2"]
                })
            ),
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "cp",
                    "args": ["target/wasm32-wasip2/debug/self_built.wasm", "tool-selfbuilt.wasm"]
                })
            ),
            format!(
                "fs|{}",
                serde_json::json!({
                    "op": "write",
                    "path": "tool-selfbuilt.manifest.toml",
                    "contents": manifest_toml()
                })
            ),
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "sh",
                    "args": ["-c", "sha256sum tool-selfbuilt.wasm || shasum -a 256 tool-selfbuilt.wasm"]
                })
            ),
            format!(
                "ext-install|{}",
                serde_json::json!({
                    "path": component.display().to_string(),
                    "allow-unsigned": true,
                    "sha256": DIGEST
                })
            ),
            "self-built|{}".to_string(),
        ])),
        named: Mutex::new(Vec::new()),
        results: Mutex::new(Vec::new()),
        digest: Mutex::new(None),
        completions: AtomicU32::new(0),
    })
}

#[test]
fn an_agent_compiles_installs_and_calls_its_own_tool_in_one_session() {
    if !common::guests_staged(&NEEDED) {
        return;
    }
    if !common::tool_available("cargo") {
        eprintln!("skipping: cargo is not on PATH");
        return;
    }
    let Some((guard, config, ws)) = fixture() else {
        return;
    };
    let ext = guard.0.join("ext");
    let component = ws.join("tool-selfbuilt.wasm");

    let script = chain_script(&component);

    let mut runtime = Runtime::boot(&config, &ext)
        .expect("the runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let factory = provider(&script);
    let mut agent = runtime.build_agent(&factory).expect("the agent boots");

    for turn in [
        "compile it",
        "copy it",
        "describe it",
        "hash it",
        "install it",
    ] {
        agent.run("s1", turn);
    }

    assert!(
        ws.join("target/wasm32-wasip2/debug/self_built.wasm")
            .exists(),
        "the compile produced nothing: {:?}",
        script.results.lock().expect("not poisoned")
    );
    assert!(
        component.exists(),
        "the artefact was not put where it could be installed"
    );
    assert!(
        ws.join("tool-selfbuilt.manifest.toml").exists(),
        "the manifest the installer requires was never written"
    );
    let digest = script
        .digest
        .lock()
        .expect("not poisoned")
        .clone()
        .expect("the agent could not obtain a digest, which is the step #221 doubted");

    let installed = agent.take_installed();
    assert_eq!(
        installed,
        vec!["tool-selfbuilt".to_string()],
        "the install left no note, so nothing downstream can adopt it; digest was {digest}"
    );
    for stem in &installed {
        let outcome = runtime.adopt_installed(stem);
        match &outcome {
            Ok(id) => agent.record_load("s1", stem, Ok(id.as_str())),
            Err(err) => agent.record_load("s1", stem, Err(&err.to_string())),
        }
        outcome.unwrap_or_else(|err| panic!("adopting what it built failed: {err}"));
    }
    drop(agent);
    let mut agent = runtime.build_agent(&factory).expect("the agent rebuilds");

    agent.run("s1", "now use it");

    let named = script.named.lock().expect("not poisoned").clone();
    assert_eq!(
        named,
        vec!["shell", "shell", "fs", "shell", "ext-install", "self-built"],
        "the chain did not run in the shape the script set"
    );
    let last = script
        .results
        .lock()
        .expect("not poisoned")
        .last()
        .cloned()
        .unwrap_or_default();
    assert!(
        last.contains(PHRASE),
        "the tool the agent compiled this session did not answer: {last}"
    );
}
