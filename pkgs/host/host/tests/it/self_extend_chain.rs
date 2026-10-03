//! Compile, install, load, call — one session, no restart (#221).
//!
//! Phase 25's exit gate. Every link had a test; the chain had none, and the
//! two sets were disjoint: `self_extend.rs` compiles and stops,
//! `install_then_call.rs` starts from a component someone else built. A
//! chain of individually-tested links is not a tested chain.
//!
//! # What the scripted model does
//!
//! Six turns, in the `self-extend` distribution's posture — its `execution:`
//! block spliced from the shipped file, its sandbox, its `install-tool`
//! grant:
//!
//! 1. `shell` — `cargo build --offline --locked --target wasm32-wasip2`
//! 2. `shell` — copy the artefact somewhere installable
//! 3. `fs` — **write the manifest**, which the issue did not anticipate and
//!    the installer requires: "a component nobody can inspect is not
//!    installable"
//! 4. `shell` — `shasum`, because an unsigned component needs its digest
//! 5. `ext-install` — `path`, `allow-unsigned`, and **the digest read out of
//!    turn 4's own output**, which is the step that existed only in theory
//! 6. the new tool, by name
//!
//! Nothing is stubbed and no step is assumed. In particular the digest is
//! not computed by the test and handed to the installer: it is parsed from
//! what the shell tool returned, the way a model would have to.
//!
//! # Why it skips
//!
//! Compiling a real component needs a prepared crate — `make
//! self-extend-fixture`, which locks and fetches once, online. The skip is
//! loud and names the command, like `polyglot_ts.rs`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;
use crate::self_extend::shipped_execution_block;

/// The guests the chain needs staged before any of it can run.
const NEEDED: [&str; 4] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-shell.wasm",
    "tool-fs.wasm",
];

/// What turn 6 asserts on: the fixture's own answer, which can only reach
/// the model if every link ran.
const PHRASE: &str = "compiled-in-session";

/// Where turn 5's digest is spliced in.
const DIGEST: &str = "{{sha256}}";

/// The scripted model: a queue of `name|arguments`, and what came back.
struct Script {
    /// Popped front to back, one per turn.
    calls: Mutex<std::collections::VecDeque<String>>,
    /// Tool names called, in order, so the chain's shape is checkable.
    named: Mutex<Vec<String>>,
    /// Every request body that carried a tool result.
    results: Mutex<Vec<String>>,
    /// The digest, as read out of the shell tool's own output.
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

fn provider(script: &Arc<Script>) -> impl Fn() -> jan_klod_core::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jan_klod_core::route::HttpFn {
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
                // The model reads the tool's output. The digest is in there
                // exactly once the shell has printed it, and nowhere else.
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

fn done() -> jan_klod_core::http::WireResponse {
    reply(
        &serde_json::json!({"choices":[{"message":{"role":"assistant",
        "content":"done"},"finish_reason":"stop"}]}),
    )
}

fn reply(value: &serde_json::Value) -> jan_klod_core::http::WireResponse {
    jan_klod_core::http::WireResponse {
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

    // The agent's starting point: a source tree, as the issue allows —
    // "scaffolds or is given a source tree".
    for file in ["Cargo.toml", "Cargo.lock"] {
        std::fs::copy(source.join(file), ws.join(file)).expect("copies the crate");
    }
    std::fs::copy(source.join("src/lib.rs"), ws.join("src/lib.rs")).expect("copies the source");
    for entry in std::fs::read_dir(source.join("wit")).expect("reads the fixture's wit") {
        let entry = entry.expect("a wit file");
        std::fs::copy(entry.path(), ws.join("wit").join(entry.file_name())).expect("copies it");
    }

    // The fleet it starts with: no `self-built` in sight.
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

/// The manifest the agent has to write, because the installer will not
/// accept a component nobody can inspect.
///
/// `capabilities = []` is a claim, not an omission: the fixture imports
/// nothing, and the host cross-checks that against the artefact's real
/// imports — a manifest claiming less than the component asks for is
/// refused.
fn manifest_toml() -> String {
    format!(
        "name = \"tool-selfbuilt\"\nversion = \"0.0.0\"\napi-version = \"{api}\"\n\
         kind = \"tool\"\ndescription = \"built in session\"\ncapabilities = []\n",
        api = jan_klod_core::manifest::API_VERSION
    )
}

/// The six calls, in order, as the scripted model makes them.
fn chain_script(component: &std::path::Path) -> Arc<Script> {
    Arc::new(Script {
        calls: Mutex::new(std::collections::VecDeque::from(vec![
            // 1. compile, in the jail, offline.
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "cargo",
                    "args": ["build", "--offline", "--locked", "--target", "wasm32-wasip2"]
                })
            ),
            // 2. put it where a manifest can sit beside it under its own name.
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "cp",
                    "args": ["target/wasm32-wasip2/debug/self_built.wasm", "tool-selfbuilt.wasm"]
                })
            ),
            // 3. the manifest, written through the file tool.
            format!(
                "fs|{}",
                serde_json::json!({
                    "op": "write",
                    "path": "tool-selfbuilt.manifest.toml",
                    "contents": manifest_toml()
                })
            ),
            // 4. the digest. `shasum` on macOS, `sha256sum` on Linux — the
            //    model would have to find out too, and either printing it
            //    is enough.
            format!(
                "shell|{}",
                serde_json::json!({
                    "command": "sh",
                    "args": ["-c", "sha256sum tool-selfbuilt.wasm || shasum -a 256 tool-selfbuilt.wasm"]
                })
            ),
            // 5. install it. `{{sha256}}` is replaced with what turn 4
            //    printed — the test never computes it.
            format!(
                "ext-install|{}",
                serde_json::json!({
                    "path": component.display().to_string(),
                    "allow-unsigned": true,
                    "sha256": DIGEST
                })
            ),
            // 6. call the tool that did not exist when this session started.
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
    // The stem is `<category>-<kind>`: `adopt_installed` reads the category
    // from it, so a component named `self-built` would be adopted into a
    // category called `self` and never become a tool.
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

    // Each link, checked where it happened rather than inferred from the
    // end: a chain test that only asserts the last step cannot say which
    // link broke.
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

    // The seam a host surface drives between turns.
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
