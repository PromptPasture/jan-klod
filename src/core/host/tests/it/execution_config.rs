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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::sandbox;
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
    run_command_in(None, execution, command, args)
}

/// [`run_command`] with the `workspace:` line under the test's control.
///
/// `None` is the real temporary workspace every test above wants. `Some(root)`
/// splices that path in instead, which is the only way to reach the "enabled,
/// but no workspace" denial from config: [`Runtime::open_workspace`] falls back
/// to `$PWD` when the key is *absent*, and the suite's `$PWD` is a perfectly
/// adoptable directory, so leaving the key out would hand the runner a
/// workspace rather than withhold one.
fn run_command_in(root: Option<&str>, execution: &str, command: &str, args: &[&str]) -> ProbeTurn {
    let dir = std::env::temp_dir().join(format!(
        "jk-execcfg-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    let ws = root.map_or_else(|| workspace.display().to_string(), str::to_owned);
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
"
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

    // The wrapper is named, not discovered — the same seam `sandbox_landlock.rs`
    // uses, and for the same reason. `LandlockBackend` confines by re-executing
    // `current_exe()`, which in production is `jan-klod-gateway` (the binary that
    // handles the `confine` subcommand) and under nextest is *this test binary*,
    // which answers `error: Unrecognized option: 'writable'`. Every command then
    // fails in a way that reads exactly like Landlock refusing it, and the four
    // tests below that assert a command *runs* failed on Linux for that reason
    // alone ([#124](https://github.com/PromptPasture/jan-klod/issues/124)).
    //
    // Ignored on macOS, where Seatbelt shells out to `/usr/bin/sandbox-exec` and
    // no binary is re-executed. Naming it unconditionally keeps one code path
    // for both platforms.
    let runtime = Runtime::boot(&config, common::repo_root().join("ext"))
        .expect("runtime boots")
        .with_sandbox_wrapper(env!("CARGO_BIN_EXE_jan-klod-gateway"));
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

// ---------------------------------------------------------------------------
// The denials, which are the branch's real job.
//
// Each of these runs the **same command** as
// `the_execution_block_builds_a_runner_that_runs_a_command` above, which is
// therefore their shared control: that test is what says `echo from-config`
// reaches the model when nothing refuses it, so a run here that does not
// produce it was refused rather than merely broken. They differ from it by one
// config key each.
//
// Every assertion is on the **effect** — the command did not run — and not on
// the warning text, because a branch can print a warning and then carry on,
// which is the shape of bug this issue exists to catch.
// ---------------------------------------------------------------------------

/// `enabled: true` with no usable workspace still denies: a command's cwd is
/// jailed to the workspace, so without one there is nowhere to run.
///
/// The root is one that cannot be opened, per `run_command_in` — an absent
/// `workspace:` key adopts `$PWD` instead of withholding a workspace, so it
/// would not exercise this at all.
#[test]
fn enabled_without_a_usable_workspace_denies_the_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command_in(
        Some("/jan-klod-no-such-workspace"),
        "execution:\n  enabled: true",
        "echo",
        &["from-config"],
    );
    assert!(
        !turn.produced("from-config"),
        "`enabled: true` without a workspace must deny: {}",
        turn.tool_result
    );
}

/// An `execution.sandbox` that cannot be read denies rather than running the
/// command unconfined.
///
/// One line of code stands between those two outcomes, and the wrong one grants
/// more than the operator wrote: they asked for something specific about a
/// command's effects, and a misspelt mode is the likeliest way to ask for it
/// wrongly.
#[test]
fn an_unreadable_sandbox_block_denies_rather_than_running_unconfined() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command(
        "execution:\n  enabled: true\n  sandbox:\n    mode: definitely-not-a-mode",
        "echo",
        &["from-config"],
    );
    assert!(
        !turn.produced("from-config"),
        "an unparseable `execution.sandbox` must deny, not run unconfined: {}",
        turn.tool_result
    );
}

/// `require: true` that cannot be satisfied denies — the operator said they
/// would rather no command ran than one ran unconfined.
///
/// Asked for here as `mode: approval-only` with `require: true`, which is the
/// contradiction that refuses on **every** platform. The other way in — `mode:
/// os` with no backend — depends on the host having no sandbox mechanism, so on
/// a machine with one it cannot be reached; both arrive at the same
/// `policy.refusal(&effective)` line in `open_process_runner`, and this one is
/// the half that a CI runner of any OS will actually execute.
#[test]
fn an_unsatisfiable_require_denies_the_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_command(
        "execution:\n  enabled: true\n  sandbox:\n    mode: approval-only\n    require: true",
        "echo",
        &["from-config"],
    );
    assert!(
        !turn.produced("from-config"),
        "`require: true` with `mode: approval-only` must deny: {}",
        turn.tool_result
    );
}

// ---------------------------------------------------------------------------
// The confinement, which is the half an operator most needs to be true.
//
// `open_process_runner` ends in `match (effective.mode, backend)`, and the
// comment above it claims "the runtime cannot report `Os` while running commands
// unconfined". Nothing checked that: `sandbox_seatbelt.rs`, `sandbox_landlock.rs`
// and `sandbox_boundary.rs` all build their runner by hand, so they prove a
// `ProcessRunner` can be confined and say nothing about whether the boot path
// confines the one it builds.
//
// The pair below is the same shape `sandbox_seatbelt.rs` uses, moved onto the
// config path: an unconfined control first, because "the command failed" is
// otherwise indistinguishable from a missing binary or a typo in the test.
// ---------------------------------------------------------------------------

/// A directory outside any workspace, with the escape target inside it.
///
/// Its own temp tree rather than a sibling of the harness's: `run_command_in`
/// deletes everything it created when the turn ends, so a target under there
/// would be gone before a test could reason about it.
fn escape_target(tag: &str) -> (PathBuf, common::TempDir) {
    let dir = std::env::temp_dir().join(format!("jk-execcfg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the directory outside the workspace");
    (dir.join("leak"), common::TempDir(dir))
}

/// `sh -c "echo x > TARGET && echo wrote"` through the usual harness.
///
/// The observation is **`wrote` reaching the model**, not the file existing:
/// `&&` makes the word conditional on the write, and a word on the wire survives
/// the harness tearing its temp tree down, which a file does not.
fn write_outside(execution: &str, target: &Path) -> ProbeTurn {
    let script = format!("echo x > {} && echo wrote", target.display());
    run_command(execution, "sh", &["-c", &script])
}

/// The control, and it runs everywhere: `mode: approval-only` wires no backend
/// on any platform, so the escape write succeeds.
///
/// Platform-independent on purpose. The confined half below cannot run without a
/// backend, but this half must, or a machine with no backend would report a
/// green suite having checked neither side.
#[test]
fn an_approval_only_command_from_config_can_write_outside_the_workspace() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let (target, _guard) = escape_target("unconfined");
    let turn = write_outside(
        "execution:\n  enabled: true\n  sandbox:\n    mode: approval-only",
        &target,
    );
    assert!(
        turn.produced("wrote"),
        "unconfined, the escape write must succeed — otherwise the denial below \
         proves nothing: {}",
        turn.tool_result
    );
}

/// The claim itself: a runner built **from config** is confined by the backend
/// the same config resolved.
///
/// Asked for by writing nothing — no `sandbox:` block at all, so `mode` is the
/// `Os` default and `writable` is the workspace. That is what an operator gets
/// for asking for nothing, which is the configuration most of them will run.
///
/// Gated on the host having a backend rather than on `#[cfg]`: `mode: os`
/// downgrades to approval-only where there is none, so without a backend this
/// asserts the opposite of what it says. The Linux backend can also be absent at
/// runtime on a kernel without Landlock, which a `cfg` would not notice.
#[test]
fn a_config_built_runner_is_confined_by_the_backend_it_resolved() {
    if !common::guests_staged(&GUESTS) || sandbox::host_backend().is_none() {
        return;
    }
    let (target, _guard) = escape_target("confined");
    let turn = write_outside("execution:\n  enabled: true", &target);
    assert!(
        !turn.produced("wrote"),
        "a command from a default `execution:` block must be confined to the \
         workspace: {}",
        turn.tool_result
    );
}
