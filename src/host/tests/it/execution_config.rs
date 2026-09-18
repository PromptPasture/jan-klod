//! Config `execution:` block end-to-end. Other tests build `ProcessRunner` by
//! hand, leaving `Runtime::open_process_runner` untested (#82). Boots real
//! `Runtime`, drives command through `tool-proc-probe` (Component-Model
//! boundary). Provider canned. Skips when guests not staged in `ext/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jan_klod_core::sandbox;
use jan_klod_core::Runtime;

use crate::common;

/// The guests a turn through `proc-probe` needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "tool-proc-probe.wasm"];

/// What a turn's command did as the *model* saw it. `tool_result` pulled from
/// completion body following tool call — the only external visibility point.
/// `AgentSession` exposes `run`, not fleet. Wire sourcing = real path.
pub struct ProbeTurn {
    /// Follow-up request body carrying tool's output.
    pub tool_result: String,
    /// Completions the turn asked for. One = no tool call, turn gave up.
    pub completions: u32,
}

impl ProbeTurn {
    /// Command ran and output reached model.
    pub fn produced(&self, needle: &str) -> bool {
        self.tool_result.contains(needle)
    }
}

/// Boot `Runtime` with `execution` config, drive one `proc-probe` command.
/// `execution` spliced verbatim; tests write real operator YAML, including
/// omitting it (the default this overrides).
fn run_command(execution: &str, command: &str, args: &[&str]) -> ProbeTurn {
    run_command_in(None, execution, command, args)
}

/// [`run_command`] with `workspace:` under test control. `None` = real temp
/// workspace. `Some(root)` = forced path, only way to reach "enabled but no
/// workspace" denial: [`Runtime::open_workspace`] falls back to `$PWD` absent,
/// so omitting key adopts `$PWD` rather than withholding.
pub fn run_command_in(
    root: Option<&str>,
    execution: &str,
    command: &str,
    args: &[&str],
) -> ProbeTurn {
    run_arguments_in(
        root,
        execution,
        &serde_json::json!({ "command": command, "args": args }),
    )
}

/// Ask `tool-proc-probe` for a long-lived child by name (#109).
fn run_spawn(execution: &str, name: &str) -> ProbeTurn {
    run_arguments_in(None, execution, &serde_json::json!({ "spawn": name }))
}

/// [`run_spawn`] but guest doesn't kill child — the "guest forgets" case the
/// host's lifetime guarantee covers.
fn run_spawn_and_leak(execution: &str, name: &str) -> ProbeTurn {
    run_arguments_in(
        None,
        execution,
        &serde_json::json!({ "spawn": name, "leak": true }),
    )
}

/// [`run_spawn`] then write `send` to child and read response.
fn run_spawn_echo(execution: &str, name: &str, send: &str) -> ProbeTurn {
    run_arguments_in(
        None,
        execution,
        &serde_json::json!({ "spawn": name, "send": send }),
    )
}

/// [`run_command_in`] with full tool arguments. Probe takes multi-shaped
/// requests (command or child), harness passes through not assembling one.
fn run_arguments_in(
    root: Option<&str>,
    execution: &str,
    arguments: &serde_json::Value,
) -> ProbeTurn {
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
    let arguments = arguments.to_string();
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

    // Completion 0 requests command; 1 delivers result and ends turn.
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

    // Wrapper named not discovered (like sandbox_landlock.rs). `LandlockBackend`
    // re-executes `current_exe()`: prod=`jan-klod-gateway`, nextest=test binary
    // answering 'Unrecognized option writable'. Commands fail like Landlock
    // refusing (#124). Ignored on macOS (Seatbelt `/usr/bin/sandbox-exec`).
    // One code path both platforms.
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

// The values, not just the switch. All `proc-error` map to
// `ToolError::ExecutionFailed`, so timeout, denial, and spawn failure are
// indistinguishable. Each test runs the same command twice, changing only the
// config value under test: success is control, failure proves the value
// causes denial.

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

// Denials verify the branch's job. Each runs the same `echo from-config`
// command (the control test above) with one config value changed. Assertions
// test the effect (command didn't run), not the warning text, because bugs can
// print a warning and carry on, which is what this issue exists to catch.

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

// Confinement tests verify operator's core need: boot path confines.
// `sandbox_seatbelt.rs`, `sandbox_landlock.rs`, and `sandbox_boundary.rs`
// build `ProcessRunner` by hand, so they prove a `ProcessRunner` can be
// confined and say nothing about whether the config boot path confines it.
// Control test unconfined first (to distinguish failure from typo), then
// confined.

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

/// Confinement still **permits** what policy grants. A confined runner that
/// denies everything would pass the escape test ([#124](https://github.com/PromptPasture/jan-klod/issues/124)): "denied" and "never ran" are
/// identical from outside. Only confined *and* succeeding separates them. The
/// write is relative, landing in the workspace the runner jails to. Gated on
/// backend to avoid asserting an *unconfined* command can write.
#[test]
fn a_confined_config_built_runner_can_still_write_inside_the_workspace() {
    if !common::guests_staged(&GUESTS) || sandbox::host_backend().is_none() {
        return;
    }
    let turn = run_command(
        "execution:\n  enabled: true",
        "sh",
        &["-c", "echo inside-ok > allowed.txt && cat allowed.txt"],
    );
    assert!(
        turn.produced("inside-ok"),
        "a confined command must still write where the policy grants it — \
         otherwise the escape test above passes for a runner that denies \
         everything: {}",
        turn.tool_result
    );
}

// Long-lived children (#109). The grant is `execution.long-lived`, which
// names *processes* rather than permitting spawning — so the thing to prove is
// that a guest's own string never becomes a program. Asserted across the
// Component-Model boundary through `tool-proc-probe`'s `{"spawn": name}`
// argument, because the guest is the party being distrusted.

/// A config with one granted long-lived child.
const GRANTED: &str =
    "execution:\n  enabled: true\n  long-lived:\n    - name: echoer\n      command: cat\n";

/// A name the operator did not write down cannot be spawned.
///
/// Its control is `a_granted_long_lived_child_starts_and_answers` below, and it
/// is not optional: without a granted name that *works*, this passes against a
/// host that refuses everything.
#[test]
fn an_unnamed_long_lived_child_is_refused() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_spawn(GRANTED, "not-in-the-config");
    assert!(
        turn.produced("spawn-refused"),
        "a child the operator never named must be refused: {}",
        turn.tool_result
    );
}

/// Grant is read from config. Control without which the test above passes
/// against a host that refuses everything. Asserts refusal's absence, not a
/// handle. Box 3 starts the child; what distinguishes them is the host log.
#[test]
fn the_long_lived_grant_is_read_from_config() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let policy = jan_klod_core::host_process::ProcessRunner::disabled().with_long_lived(vec![
        jan_klod_core::host_process::LongLived {
            name: "echoer".to_owned(),
            command: "cat".to_owned(),
            args: vec![],
        },
    ]);
    assert!(
        policy.long_lived_grant("echoer").is_some(),
        "a named child is grantable"
    );
    assert!(
        policy.long_lived_grant("not-in-the-config").is_none(),
        "and an unnamed one is not"
    );
}

/// The control the refusal test needs: a **granted** name starts a real
/// process, is written to, and answers.
///
/// Without this, `an_unnamed_long_lived_child_is_refused` passes against a host
/// that refuses everything. A handle alone would not be enough either: it proves
/// a number was issued, not that anything is on the other end of it. So the
/// child is `cat`, the test writes a line, and the assertion is that the line
/// comes back.
#[test]
fn a_granted_long_lived_child_starts_and_answers() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_spawn_echo(GRANTED, "echoer", "ping-back\n");
    assert!(
        turn.produced("ping-back"),
        "a granted child must start and echo what it was sent: {}",
        turn.tool_result
    );
    assert!(
        !turn.produced("spawn-refused"),
        "and must not be refused: {}",
        turn.tool_result
    );
}

/// Acceptance line 3: the child is confined by the same backend as a one-shot
/// command.
///
/// A long-lived child outlives its call, so it is *more* exposed than a
/// one-shot one — this must not be inherited by assumption. Same escape-write
/// shape as the one-shot pair above, with `mode: approval-only` as the control,
/// because a confined child that cannot start at all would satisfy the denial
/// on its own.
#[test]
fn a_long_lived_child_is_confined_like_a_one_shot_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let (target, _guard) = escape_target("long-lived");
    let escape = format!("echo x > {} && echo wrote", target.display());
    let grant = |sandbox: &str| {
        format!(
            "execution:\n  enabled: true\n{sandbox}  long-lived:\n    - name: escaper\n      \
             command: sh\n      args: [\"-c\", \"{escape}\"]\n"
        )
    };

    // Control first: unconfined, the escape write succeeds.
    let unconfined = run_spawn(&grant("  sandbox:\n    mode: approval-only\n"), "escaper");
    assert!(
        unconfined.produced("wrote"),
        "unconfined, the child's escape write must succeed — otherwise the \
         denial below proves nothing: {}",
        unconfined.tool_result
    );

    if sandbox::host_backend().is_none() {
        return;
    }
    let confined = run_spawn(&grant(""), "escaper");
    assert!(
        !confined.produced("wrote"),
        "a long-lived child must be confined to the workspace like any other \
         command: {}",
        confined.tool_result
    );
}

/// Acceptance line 2: a child does not outlive the runtime that started it,
/// **even when the guest never kills it**.
///
/// Asserted by looking for the process, not by reading the code: "we call kill"
/// and "the child is dead" are different claims and only the second is the
/// requirement. The child reports its own pid — `echo $$` — because the host
/// never hands one to the guest and the test has no other way to learn it.
///
/// `run_arguments_in` drops the `AgentSession`, and with it the `Store` holding
/// the instance's `ToolHost`, before it returns. So by the time this test reads
/// `turn.tool_result` the runtime is already gone, and the pid either is or is
/// not still there.
///
/// The **trapping** guest the box also names reduces to this same assertion: a
/// trap poisons the instance but does not drop the `Store`, so the child
/// survives exactly until the runtime goes — which is the moment this checks.
#[test]
fn a_long_lived_child_does_not_outlive_the_runtime() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // `sleep`, not `cat`, and that choice is the test. A `cat` would exit on its
    // own the moment the host dropped its stdin pipe — so the pid would be gone
    // either way and this would pass without the host ever killing anything.
    // `sleep` ignores stdin, so it is still there in thirty seconds unless
    // something kills it.
    //
    // `exec` replaces the shell, so the pid printed is the pid that stays.
    // Without it the shell would fork and the test would watch the wrong
    // process.
    let grant = "execution:\n  enabled: true\n  long-lived:\n    - name: reporter\n      \
                 command: sh\n      args: [\"-c\", \"echo $$; exec sleep 30\"]\n";
    let turn = run_spawn_and_leak(grant, "reporter");

    let pid: u32 = turn
        .tool_result
        .split("out=")
        .nth(1)
        .and_then(|tail| {
            tail.split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
        })
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("the child must report its pid: {}", turn.tool_result));

    assert!(
        !pid_alive(pid),
        "pid {pid} is still running after the runtime was dropped — the guest \
         never killed it, so the host had to"
    );
}

/// Whether a pid is still there, asked of the OS rather than of our own
/// bookkeeping — the distinction acceptance line 2 turns on.
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|out| out.status.success())
}
