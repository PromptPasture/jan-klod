//! `registry-mcp` over a stdio child with a shell-script server; proves framing and plumbing only.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jk_agent::Runtime;

use crate::common;

const GUESTS: [&str; 2] = ["provider-openai.wasm", "registry-mcp.wasm"];

/// Per-test argv marker so `pgrep` cannot match another test's child.
fn marker(tag: &str) -> String {
    format!("jk-stdio-fixture-110-{tag}-{}", std::process::id())
}

fn fixture_script(tag: &str) -> String {
    let reply =
        |result: &str| format!(r#"printf '{{"jsonrpc":"2.0","id":1,"result":{result}}}\n'"#);
    format!(
        "MARKER={m}; while IFS= read -r line; do case \"$line\" in \
         *'\"initialize\"'*) {init} ;; \
         *'\"tools/list\"'*) {list} ;; \
         *'\"tools/call\"'*) {call} ;; \
         esac; done; sleep 30",
        m = marker(tag),
        init = reply(r#"{"protocolVersion":"2024-11-05"}"#),
        list = reply(
            r#"{"tools":[{"name":"echo-fixture","description":"a fixture tool","inputSchema":{"type":"object"}}]}"#
        ),
        call = reply(r#"{"content":[{"type":"text","text":"fixture-answered"}]}"#),
    )
}

fn config_yaml_with(workspace: &str, script: &str) -> String {
    let script = script.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  registry:
    mcp:
      enabled: true
      servers:
        - name: fixture
          transport: stdio
workspace: {workspace}
execution:
  enabled: true
  long-lived:
    - name: fixture
      command: sh
      args: ["-c", "{script}"]
"#
    )
}

fn booted(tag: &str) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    booted_with(tag, || common::canned_http("ok"))
}

fn booted_with(
    tag: &str,
    http: impl Fn() -> jk_wasm::route::HttpFn,
) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    booted_against(tag, &fixture_script(tag), http)
}

fn booted_against(
    tag: &str,
    script: &str,
    http: impl Fn() -> jk_wasm::route::HttpFn,
) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-regstdio-{tag}-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).expect("creates the workspace");
    let guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        config_yaml_with(&workspace.display().to_string(), script),
    )
    .expect("writes the config");

    let runtime = Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((guard, agent))
}

fn fixture_running(tag: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(marker(tag))
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn a_stdio_server_named_in_config_lists_its_tools() {
    let Some((_dir, mut agent)) = booted("list") else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata resolves");
    assert!(
        metas.to_string().contains("echo-fixture"),
        "the fixture server's tool must reach the fleet over stdio: {metas}"
    );
}

#[test]
fn a_tool_on_a_stdio_server_can_be_called() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let completions = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(String::new()));
    let (counter, recorded) = (Arc::clone(&completions), Arc::clone(&seen));
    let factory = move || -> jk_wasm::route::HttpFn {
        let counter = Arc::clone(&counter);
        let recorded = Arc::clone(&recorded);
        Box::new(move |_m, _u, _h, body, _t| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            if n > 0 {
                if let Ok(mut log) = recorded.lock() {
                    log.push_str(&String::from_utf8_lossy(body.unwrap_or_default()));
                }
            }
            let response = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"fixture::echo-fixture","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    };

    let Some((_dir, mut agent)) = booted_with("call", factory) else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata resolves");
    assert!(
        metas.to_string().contains("fixture::echo-fixture"),
        "{metas}"
    );

    let _ = agent.run("s1", "use the fixture tool");
    let body = seen.lock().expect("not poisoned").clone();
    assert!(
        body.contains("fixture-answered"),
        "the child's answer must reach the model: {body}"
    );
}

#[test]
fn the_stdio_child_is_gone_once_the_runtime_stops() {
    let Some((dir, mut agent)) = booted("lifetime") else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata resolves");
    assert!(
        metas.to_string().contains("echo-fixture"),
        "the child has to have started for its death to mean anything: {metas}"
    );
    assert!(
        fixture_running("lifetime"),
        "the fixture server should be running while the agent holds it"
    );

    drop(agent);
    drop(dir);

    assert!(
        !fixture_running("lifetime"),
        "the stdio child must not outlive the runtime that started it"
    );
}

const MUST_FINISH_WITHIN: std::time::Duration = std::time::Duration::from_secs(40);

fn malformed_script(tag: &str) -> String {
    format!(
        "MARKER={m}; while IFS= read -r line; do printf 'this is not json\\n'; \
         done; sleep 30",
        m = marker(tag)
    )
}

fn silent_script(tag: &str) -> String {
    format!("MARKER={m}; sleep 60", m = marker(tag))
}

#[test]
fn a_malformed_reply_leaves_the_server_down_rather_than_breaking_the_turn() {
    let started = std::time::Instant::now();
    let Some((_dir, mut agent)) =
        booted_against("malformed", &malformed_script("malformed"), || {
            common::canned_http("ok")
        })
    else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata still resolves");
    assert!(
        fixture_running("malformed"),
        "the fixture must be running and merely unparseable"
    );
    assert!(
        !metas.to_string().contains("echo-fixture"),
        "a server that cannot be parsed offers no tools: {metas}"
    );
    let answer = agent.run("s1", "hello");
    assert!(
        matches!(answer, jk_agent::conductor::RunResult::Answered { .. }),
        "a turn must still run with a broken MCP server configured: {answer:?}"
    );
    assert!(
        started.elapsed() < MUST_FINISH_WITHIN,
        "took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_silent_child_gives_up_instead_of_pinning_the_core() {
    let started = std::time::Instant::now();
    let Some((_dir, mut agent)) = booted_against("silent", &silent_script("silent"), || {
        common::canned_http("ok")
    }) else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata still resolves");
    assert!(
        fixture_running("silent"),
        "the fixture must be alive and merely silent"
    );
    assert!(
        !metas.to_string().contains("echo-fixture"),
        "a silent server offers no tools: {metas}"
    );
    let answer = agent.run("s1", "hello");
    assert!(
        matches!(answer, jk_agent::conductor::RunResult::Answered { .. }),
        "a turn must still run with a silent MCP server configured: {answer:?}"
    );
    assert!(
        started.elapsed() < MUST_FINISH_WITHIN,
        "a silent child must be given up on, not waited out: took {:?}",
        started.elapsed()
    );
}
