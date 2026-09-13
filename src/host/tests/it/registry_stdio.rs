//! `registry-mcp` over a stdio child, end to end (#110).
//!
//! Most MCP servers in the wild are stdio processes, so this is the transport
//! that decides whether the inbound ecosystem port reaches the ecosystem. The
//! test drives the whole path a real one takes: the host starts a granted
//! child, the guest completes the MCP handshake over its pipe, lists its tools,
//! and calls one.
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

/// A string in one test's child argv (not elsewhere) to find the process.
///
/// **Per test, not per file.** nextest runs tests in parallel, so a shared
/// marker makes `pgrep` find another test's child (the lifetime test failed
/// exactly that way).
fn marker(tag: &str) -> String {
    format!("jk-stdio-fixture-110-{tag}-{}", std::process::id())
}

/// A minimal MCP server: read a JSON-RPC line, answer it, repeat.
///
/// Answers only the three methods this test drives. Written as one `sh -c`
/// string rather than a file on disk so the fixture cannot drift from the test,
/// and so there is nothing to clean up.
///
/// **One line, not cosmetic.** Spliced into a YAML string where newlines fold
/// to spaces — a comment swallowed the whole program, the child exited before
/// reading, surfacing as `initialize failed: server exited`.
///
/// `MARKER` is a variable assignment rather than a comment for the same reason:
/// it has to be in the child's argv without disabling anything after it.
///
/// **The trailing `sleep 30` makes the lifetime test meaningful.** A stdio
/// server reads stdin, so when the host drops the pipe the loop hits EOF and
/// the shell exits *on its own* — and a test asking "is the child gone" would
/// pass whether or not anything killed it. It is the same trap #109 hit with
/// `cat`, arrived at from the other side. With `sleep` the process persists
/// unless killed; the probe confirms this (with `LiveChild::drop` emptied, the
/// test fails). Not `exec sleep` because that replaces the shell and hides
/// `MARKER`.
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

/// Config with `registry.mcp` enabled over stdio, and the child granted.
///
/// Two halves that must agree: `execution.long-lived` is the operator granting
/// a process, and the server entry is the guest naming it. The guest supplies a
/// name, never a command. `script` is written out by the caller so a test can
/// grant a server that misbehaves as easily as one that works.
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
    booted_against(tag, &fixture_script(tag), http)
}

/// [`booted_with`], against a server of the caller's choosing.
fn booted_against(
    tag: &str,
    script: &str,
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
    std::fs::write(
        &config,
        config_yaml_with(&workspace.display().to_string(), script),
    )
    .expect("writes the config");

    // Name the real wrapper binary, exactly as `execution_config.rs` does and
    // for exactly the same reason. `LandlockBackend` confines by re-executing
    // `current_exe()` — in production `jan-klod-gateway`, which handles the
    // `confine` subcommand, but under nextest *this test binary*, which answers
    // `error: Unrecognized option: 'writable'` and exits. The fixture server
    // then dies before reading a byte, which surfaces as `initialize failed:
    // server exited` (see #132, #124).
    // Named unconditionally rather than behind a `cfg`: one code path for both
    // platforms, and on macOS it is simply unused.
    let runtime = Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((guard, agent))
}

/// Whether the fixture child is running, asked of the OS by its argv rather
/// than by a pid the host never hands out.
fn fixture_running(tag: &str) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(marker(tag))
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
/// Driven through a turn (the only real path) — the tool's output is visible
/// only in the next completion's request body.
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

// ---------------------------------------------------------------------------
// Acceptance line 2: a server that misbehaves must not pin the core.
//
// The core is single-threaded. A guest that waited on a child forever would
// hold the turn, the transport and every other instance with it — so the only
// interesting question about a broken MCP server is whether asking it
// *finishes*. These assert on that, with a wall-clock bound, rather than on an
// error appearing somewhere: a client that hung indefinitely and was killed by
// the test harness would also "produce no tools".
// ---------------------------------------------------------------------------

/// Long enough for the guest's own ten-second budget plus a slow machine, short
/// enough that an unbounded wait cannot pass.
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

/// A reply that is not JSON is a server that is down, not a turn that fails.
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
    // **"Down" has to mean alive-but-unparseable, not never-started.** Both
    // this test and the silent one below assert an *absence*, and an absence is
    // what a test proves when nothing works at all — on Linux both passed
    // happily while #132 meant the child was being eaten by the sandbox wrapper
    // before it ran, which is a completely different thing from the one they
    // describe. This fixture ends in `sleep 30` precisely so it is still there
    // to be asked about.
    assert!(
        fixture_running("malformed"),
        "the fixture must be running and merely unparseable"
    );
    assert!(
        !metas.to_string().contains("echo-fixture"),
        "a server that cannot be parsed offers no tools: {metas}"
    );
    // The turn is the thing that must survive: a broken registry is a missing
    // capability, not a broken agent.
    let answer = agent.run("s1", "hello");
    assert!(
        matches!(answer, jan_klod_core::conductor::RunResult::Answered { .. }),
        "a turn must still run with a broken MCP server configured: {answer:?}"
    );
    assert!(
        started.elapsed() < MUST_FINISH_WITHIN,
        "took {:?}",
        started.elapsed()
    );
}

/// **The one this box exists for**: a child that never answers.
///
/// It starts, it stays up, and it says nothing — so `is-running` keeps
/// returning true and only the guest's own budget ends the wait. Without that
/// budget this test does not fail, it *hangs*, which is the failure this
/// acceptance line is about.
#[test]
fn a_silent_child_gives_up_instead_of_pinning_the_core() {
    let started = std::time::Instant::now();
    let Some((_dir, mut agent)) = booted_against("silent", &silent_script("silent"), || {
        common::canned_http("ok")
    }) else {
        return;
    };
    let metas = agent.all_metas_json().expect("metadata still resolves");
    // Silent means *alive and saying nothing* — the whole point of `sleep 60`.
    // A child that exited is not silent, it is absent, and the give-up path
    // this test describes is never reached. See the note in the malformed test.
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
        matches!(answer, jan_klod_core::conductor::RunResult::Answered { .. }),
        "a turn must still run with a silent MCP server configured: {answer:?}"
    );
    assert!(
        started.elapsed() < MUST_FINISH_WITHIN,
        "a silent child must be given up on, not waited out: took {:?}",
        started.elapsed()
    );
}
