//! `registry-mcp` over a stdio child, end to end (#110).
//!
//! Most MCP servers in the wild are stdio processes, so this is the transport
//! that decides whether the inbound half of the ecosystem port reaches the
//! ecosystem. The test drives the whole path a real one takes: the host starts a
//! granted child, the guest completes the MCP handshake over its pipe, lists its
//! tools, and calls one.
//!
//! **The server is a shell script, not a real MCP server**, and offline by
//! construction — there is no network anywhere in this file. What it proves is
//! the framing and the plumbing, which is what this slice adds; whether some
//! third-party server behaves is not something a test here could say.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;

/// The guests a turn through the MCP registry needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "registry-mcp.wasm"];

/// A string that appears in the child's argv and nowhere else on the machine,
/// so the process can be found without knowing its pid.
const MARKER: &str = "jk-stdio-fixture-110";

/// A minimal MCP server: read a JSON-RPC line, answer it, repeat.
///
/// Answers only the three methods this test drives. Written as one `sh -c`
/// string rather than a file on disk so the fixture cannot drift from the test
/// that uses it, and so there is nothing to clean up.
///
/// **One line, and that is not cosmetic.** It is spliced into a double-quoted
/// YAML scalar, where newlines fold to spaces — a multi-line script became
/// `# marker while IFS= read …`, the leading comment swallowed the whole
/// program, and the child exited before reading a byte. Which surfaced as
/// `initialize failed: server exited`, i.e. exactly what a broken *server*
/// looks like.
///
/// `MARKER` is a variable assignment rather than a comment for the same reason:
/// it has to be in the child's argv without disabling anything after it.
///
/// **The trailing `sleep 30` is what makes the lifetime test mean anything.**
/// A stdio server reads stdin, so when the host drops the pipe the loop hits
/// EOF and the shell exits *on its own* — and a test asking "is the child gone"
/// would pass whether or not anything killed it. It is the same trap #109 hit
/// with `cat`, arrived at from the other side. With the sleep the process is
/// still there in thirty seconds unless the host kills it, and the probe below
/// confirms that: with `LiveChild::drop` emptied, the lifetime test fails.
/// Not `exec sleep`, because that replaces the shell and takes `MARKER` out of
/// argv with it, leaving nothing for `pgrep` to find.
fn fixture_script() -> String {
    let reply =
        |result: &str| format!(r#"printf '{{"jsonrpc":"2.0","id":1,"result":{result}}}\n'"#);
    format!(
        "MARKER={MARKER}; while IFS= read -r line; do case \"$line\" in \
         *'\"initialize\"'*) {init} ;; \
         *'\"tools/list\"'*) {list} ;; \
         *'\"tools/call\"'*) {call} ;; \
         esac; done; sleep 30",
        init = reply(r#"{"protocolVersion":"2024-11-05"}"#),
        list = reply(
            r#"{"tools":[{"name":"echo-fixture","description":"a fixture tool","inputSchema":{"type":"object"}}]}"#
        ),
        call = reply(r#"{"content":[{"type":"text","text":"fixture-answered"}]}"#),
    )
}

/// Config with `registry.mcp` enabled over stdio, and the child granted.
///
/// Two halves that must agree: `execution.long-lived` is the operator granting
/// a process, and the server entry is the guest naming it. That is the whole
/// shape of the capability — the guest supplies a name, never a command.
fn config_yaml(workspace: &str) -> String {
    let script = fixture_script().replace('\\', "\\\\").replace('"', "\\\"");
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

/// Boot an agent against the fixture server. Returns the agent and the temp dir
/// guard, so the caller decides when the runtime drops — which is the point of
/// the last test below.
fn booted(tag: &str) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    booted_with(tag, || common::canned_http("ok"))
}

/// [`booted`], driven by a provider of the caller's choice.
fn booted_with(
    tag: &str,
    http: impl Fn() -> jan_klod_core::route::HttpFn,
) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-regstdio-{tag}-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).expect("creates the workspace");
    let guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    std::fs::write(&config, config_yaml(&workspace.display().to_string()))
        .expect("writes the config");

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((guard, agent))
}

/// Whether the fixture child is running, asked of the OS by its argv rather
/// than by a pid the host never hands out.
fn fixture_running() -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(MARKER)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Acceptance line 1: a stdio MCP server named in config is reachable, and
/// `tools/list` through it works.
///
/// `all_metas_json` is the first thing that asks the registry fleet for
/// anything (#59's laziness), so reaching it means the child was started, the
/// handshake completed over its pipe, and its tool list came back — the whole
/// stdio path, asserted by the one thing at the end of it.
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

/// The other half of acceptance line 1: `tools/call` through the same child.
///
/// Driven through a turn rather than by calling the fleet directly, because the
/// model naming a tool is the only path a real call takes — and the tool's
/// output is visible from outside only in the request body of the completion
/// that follows it.
#[test]
fn a_tool_on_a_stdio_server_can_be_called() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // Completion 0 asks for the fixture tool; completion 1 arrives carrying its
    // output in the request body, which is the only place a tool's result is
    // visible from outside.
    let completions = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(String::new()));
    let (counter, recorded) = (Arc::clone(&completions), Arc::clone(&seen));
    let factory = move || -> jan_klod_core::route::HttpFn {
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
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    };

    let Some((_dir, mut agent)) = booted_with("call", factory) else {
        return;
    };
    // Resolve the fleet first, so the tool exists by the time the turn names it.
    // The registry namespaces its tools by server, so the model names
    // `fixture::echo-fixture` — the server entry's name, then the tool's.
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

/// Acceptance line 1's last clause: the child is gone after the runtime stops.
///
/// This is #109's lifetime guarantee holding for a **registry** instance rather
/// than a tool one — the two adapters share one `Children` table precisely so
/// there is one guarantee rather than two implementations of it. Asserted by
/// looking for the process, as #109 did, because "we call kill" and "the child
/// is dead" are different claims.
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
        fixture_running(),
        "the fixture server should be running while the agent holds it"
    );

    drop(agent);
    drop(dir);

    assert!(
        !fixture_running(),
        "the stdio child must not outlive the runtime that started it"
    );
}
