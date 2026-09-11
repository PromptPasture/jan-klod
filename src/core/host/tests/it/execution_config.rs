//! The top-level `execution:` block, from config to a running command.
//!
//! Every other test that exercises command execution builds a `ProcessRunner`
//! by hand — `host_process.rs`, `tool_git.rs`, `sandbox_seatbelt.rs`,
//! `sandbox_landlock.rs` — so `Runtime::open_process_runner`, the only thing
//! that turns `execution:` into a runner in production, was reached by nothing
//! (#82). The assertions were real; they were not attached to the code that
//! runs.
//!
//! So this boots a real `Runtime` from a config fixture and drives a command
//! through `tool-proc-probe`, across the Component-Model boundary rather than
//! at the Rust seam. Offline: the provider is canned.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;

/// The guests a turn through `proc-probe` needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "tool-proc-probe.wasm"];

/// What one turn's command did, as the *model* saw it.
///
/// `tool_result` is pulled out of the request body of the completion that
/// follows the tool call, which is the only place a tool's output is visible
/// from outside: `AgentSession` exposes `run`, not the fleet. That it comes
/// from the wire is a feature — it is the same path a real answer takes.
struct ProbeTurn {
    /// The follow-up request body, carrying whatever the tool returned.
    tool_result: String,
    /// How many completions the turn asked for. One means the tool call never
    /// happened — the turn gave up first.
    completions: u32,
}

impl ProbeTurn {
    /// Whether the command ran and its output reached the model.
    fn produced(&self, needle: &str) -> bool {
        self.tool_result.contains(needle)
    }
}

/// Boot a `Runtime` whose config carries `execution`, then drive one
/// `proc-probe` command through a turn.
///
/// `execution` is spliced in verbatim so a test can write the real YAML an
/// operator would — including leaving it out entirely, which is the default
/// this block exists to override.
fn run_command(execution: &str, command: &str, args: &[&str]) -> ProbeTurn {
    let dir = std::env::temp_dir().join(format!(
        "jk-execcfg-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    let arguments = serde_json::json!({ "command": command, "args": args }).to_string();
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    proc-probe:
      enabled: true
workspace: {ws}
{execution}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    // Completion 0 asks for the command; completion 1 carries its result in the
    // request body and ends the turn.
    let completions = Arc::new(AtomicU32::new(0));
    let seen = Arc::new(Mutex::new(String::new()));
    let (counter, recorded) = (Arc::clone(&completions), Arc::clone(&seen));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let counter = Arc::clone(&counter);
        let recorded = Arc::clone(&recorded);
        let arguments = arguments.clone();
        Box::new(move |_m, _u, _h, body, _t| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            if n > 0 {
                if let Ok(mut log) = recorded.lock() {
                    log.push_str(&String::from_utf8_lossy(body.unwrap_or_default()));
                }
            }
            let response = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"proc-probe","arguments":arguments}}]},
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

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let _ = agent.run("s1", "run the command");

    let tool_result = seen.lock().expect("not poisoned").clone();
    ProbeTurn {
        tool_result,
        completions: completions.load(Ordering::Relaxed),
    }
}

/// `execution.enabled: true` with a workspace reaches the runner, and a command
/// runs.
///
/// The assertion is on the command's **output reaching the model**, not on the
/// turn succeeding: a denied command also finishes the turn, having told the
/// model it was refused.
#[test]
fn the_execution_block_builds_a_runner_that_runs_a_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command("execution:\n  enabled: true", "echo", &["from-config"]);
    assert!(
        turn.completions >= 2,
        "the model must have been asked again with the tool's result: {turn:?}",
        turn = turn.completions
    );
    assert!(
        turn.produced("from-config"),
        "a command configured through `execution:` must run and reach the model: {}",
        turn.tool_result
    );
}

/// The other side of the same branch: no `execution:` block at all denies.
///
/// Without this the test above would pass against a runner built by any means —
/// including one that ignored config entirely — so this is what makes the pair
/// say something about `open_process_runner` rather than about `ProcessRunner`.
#[test]
fn no_execution_block_denies_the_same_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command("", "echo", &["from-config"]);
    assert!(
        !turn.produced("from-config"),
        "with no `execution:` block the command must not run: {}",
        turn.tool_result
    );
}

// ---------------------------------------------------------------------------
// The values, not just the switch.
//
// `tool-proc-probe` maps **every** `proc-error` to `ToolError::ExecutionFailed`
// with an empty message, so a timeout, a denial and a spawn failure are one
// thing by the time they reach the model. A lone "it failed" assertion would
// therefore pass for any of those reasons — including the config never being
// read at all. So each of these runs the **same command** twice, changing only
// the one config value under test: the run that succeeds is the control, and it
// is what makes the run that fails mean what it says.
// ---------------------------------------------------------------------------

/// Two seconds, then a word to look for. Slow enough to outlast a one-second
/// budget, short enough to sit under a generous one.
const SLOW: (&str, [&str; 2]) = ("sh", ["-c", "sleep 2; echo slept"]);

/// The control for the timeout pair: with room to finish, it finishes.
#[test]
fn a_slow_command_finishes_when_the_configured_timeout_allows_it() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command(
        "execution:\n  enabled: true\n  timeout-secs: 30",
        SLOW.0,
        &SLOW.1,
    );
    assert!(
        turn.produced("slept"),
        "the control must succeed, or the timeout test below proves nothing: {}",
        turn.tool_result
    );
}

/// `timeout-secs` from config bounds the command. Same command as the control
/// above, so the only difference is the number in the config.
#[test]
fn timeout_secs_from_config_stops_a_slow_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command(
        "execution:\n  enabled: true\n  timeout-secs: 1",
        SLOW.0,
        &SLOW.1,
    );
    assert!(
        !turn.produced("slept"),
        "`timeout-secs: 1` must stop a two-second command: {}",
        turn.tool_result
    );
}

/// `output-cap` from config truncates, and says so.
///
/// This one carries its own evidence — the runner appends `…[truncated]`, which
/// no other failure produces — but it still runs the pair, because a cap that
/// was ignored and a command that produced nothing look identical otherwise.
#[test]
fn output_cap_from_config_truncates_a_long_result() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let long = "A".repeat(200);
    let capped = run_command(
        "execution:\n  enabled: true\n  output-cap: 16",
        "echo",
        &[&long],
    );
    assert!(
        capped.produced("[truncated]"),
        "`output-cap: 16` must truncate 200 bytes of output: {}",
        capped.tool_result
    );

    let uncapped = run_command("execution:\n  enabled: true", "echo", &[&long]);
    assert!(
        !uncapped.produced("[truncated]"),
        "the default cap must not truncate the same output: {}",
        uncapped.tool_result
    );
}

/// `env-passthrough` is a **grant**, one name at a time — not a switch that
/// hands a child the parent's environment.
///
/// Both halves come from one `env` listing, which is what makes the absence
/// meaningful: the same output that shows the granted value would have shown
/// the ungranted one. Values are deliberately non-overlapping, so neither can
/// satisfy the other's assertion by being a substring of it.
#[test]
fn env_passthrough_grants_one_name_and_not_the_rest() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    std::env::set_var("JK_EXEC_GRANTED", "let-me-in");
    std::env::set_var("JK_EXEC_UNGRANTED", "keep-me-out");

    let turn = run_command(
        "execution:\n  enabled: true\n  env-passthrough: [JK_EXEC_GRANTED]",
        "env",
        &[],
    );
    assert!(
        turn.produced("let-me-in"),
        "a granted name must reach the child: {}",
        turn.tool_result
    );
    assert!(
        !turn.produced("keep-me-out"),
        "an ungranted name must not, or `env-passthrough` is not a grant: {}",
        turn.tool_result
    );
}
