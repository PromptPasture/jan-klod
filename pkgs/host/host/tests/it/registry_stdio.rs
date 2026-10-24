//! `registry-mcp` over a stdio child, end to end (#110). The server is a shell
//! script, so this proves the framing and plumbing, not that a real MCP server
//! behaves. Offline; skips when guests are not staged.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jk_agent::Runtime;

use crate::common;

/// The guests a turn through the MCP registry needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "registry-mcp.wasm"];

/// Per-test argv marker so `pgrep` cannot match another test's child.
fn marker(tag: &str) -> String {
    format!("jk-stdio-fixture-110-{tag}-{}", std::process::id())
}

/// Minimal MCP server as one `sh -c` line, answering the three methods the test
/// drives. Must stay one line: YAML folds newlines, and a comment swallowed the
/// program. `MARKER` is an assignment, not a comment, so it stays in argv.
/// The trailing `sleep 30` keeps the child alive after stdin hits EOF; without it
/// the lifetime test would pass whether or not anything killed the child (#109).
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

/// Config enabling `registry.mcp` over stdio. The operator grants the process
/// (`long-lived`); the guest names it and never supplies a command.
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

/// Boots an agent against the fixture server. The caller owns the temp dir guard,
/// which decides when the runtime drops.
fn booted(tag: &str) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    booted_with(tag, || common::canned_http("ok"))
}

/// [`booted`], driven by a provider of the caller's choice.
fn booted_with(
    tag: &str,
    http: impl Fn() -> jk_wasm::route::HttpFn,
) -> Option<(common::TempDir, jk_agent::AgentSession)> {
    booted_against(tag, &fixture_script(tag), http)
}

/// [`booted_with`], against a server of the caller's choosing.
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

    // Use the real gateway as the sandbox wrapper, as `execution_config.rs` does.
    // Landlock re-execs `current_exe()`, which under nextest is this test binary;
    // it would exit before the handshake (#132, #124).
    let runtime = Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((guard, agent))
}

/// Whether the fixture child is running, checked by its argv marker.
fn fixture_running(tag: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(marker(tag))
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Acceptance line 1: a stdio server named in config lists its tools.
#[test]
fn a_stdio_server_named_in_config_lists_its_tools() {
    let Some((_dir, mut agent)) = booted("list") else {
        return;
    };
    // First registry call: reaching it means the child started and the handshake completed.
    let metas = agent.all_metas_json().expect("metadata resolves");
    assert!(
        metas.to_string().contains("echo-fixture"),
        "the fixture server's tool must reach the fleet over stdio: {metas}"
    );
}

/// Acceptance line 1, the other half: `tools/call` through a turn.
#[test]
fn a_tool_on_a_stdio_server_can_be_called() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // The tool's output is visible only in the next completion's request body.
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
    // Tools are namespaced `server::tool`, so the model names `fixture::echo-fixture`.
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

/// Acceptance line 1's last clause: the child dies with the runtime. Checked by
/// looking for the process, since calling kill is not the same as the child being dead (#109).
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

/// Acceptance line 2 bound: a misbehaving server must not pin the single-threaded
/// core. Long enough for the guest's own ten-second budget, short enough that an
/// unbounded wait cannot pass.
const MUST_FINISH_WITHIN: std::time::Duration = std::time::Duration::from_secs(40);

/// A server that answers every request with something that is not JSON.
fn malformed_script(tag: &str) -> String {
    format!(
        "MARKER={m}; while IFS= read -r line; do printf 'this is not json\\n'; \
         done; sleep 30",
        m = marker(tag)
    )
}

/// A server that starts, stays up, and never answers anything.
fn silent_script(tag: &str) -> String {
    format!("MARKER={m}; sleep 60", m = marker(tag))
}

/// A reply that is not JSON means the server is down, not that the turn fails.
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
    // Assert the child is alive, not absent: an absent child passed these tests
    // while #132 had the sandbox wrapper killing it before it ran.
    assert!(
        fixture_running("malformed"),
        "the fixture must be running and merely unparseable"
    );
    assert!(
        !metas.to_string().contains("echo-fixture"),
        "a server that cannot be parsed offers no tools: {metas}"
    );
    // A broken registry is a missing capability, not a broken agent.
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

/// A child that starts and never answers. Only the guest's own budget ends the
/// wait, so without it this test hangs rather than fails.
#[test]
fn a_silent_child_gives_up_instead_of_pinning_the_core() {
    let started = std::time::Instant::now();
    let Some((_dir, mut agent)) = booted_against("silent", &silent_script("silent"), || {
        common::canned_http("ok")
    }) else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata still resolves");
    // Must be alive: an exited child would skip the give-up path under test.
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
