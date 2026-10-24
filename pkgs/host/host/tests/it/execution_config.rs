//! Config `execution:` block end-to-end (#82): a real `Runtime` runs commands
//! through `tool-proc-probe` across the Component Model boundary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use jk_agent::Runtime;
use jk_wasm::sandbox;

use crate::common;

/// The guests a turn through `proc-probe` needs.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "tool-proc-probe.wasm"];

/// What a turn's command did, as the model saw it.
pub struct ProbeTurn {
    /// Follow-up request body carrying the tool's output.
    pub tool_result: String,
    /// Completions the turn asked for; one means no tool call.
    pub completions: u32,
}

impl ProbeTurn {
    /// Whether `needle` reached the model.
    pub fn produced(&self, needle: &str) -> bool {
        self.tool_result.contains(needle)
    }
}

/// Boots a `Runtime` with the given `execution:` YAML and drives one `proc-probe` command.
fn run_command(execution: &str, command: &str, args: &[&str]) -> ProbeTurn {
    run_command_in(None, execution, command, args)
}

/// [`run_command`] with `workspace:` forced to `root`. Only a forced, unopenable root reaches the "no workspace" denial.
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

/// Ask `tool-proc-probe` which long-lived names it may start (#220).
fn run_granted(execution: &str) -> ProbeTurn {
    run_arguments_in(None, execution, &serde_json::json!({ "granted": true }))
}

/// [`run_spawn`] where the guest never kills the child.
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

/// [`run_command_in`] taking the full tool arguments, since the probe accepts several request shapes.
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
    let factory = move || -> jk_wasm::route::HttpFn {
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
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    };

    // `LandlockBackend` re-execs `current_exe()`, so it must be pointed at the gateway, not the test binary (#124).
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

/// With `execution.enabled: true` a command runs. Asserts on its output reaching the model, since a denial also ends the turn.
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

/// Without an `execution:` block the same command is denied, so the pair proves the config matters.
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

// Every `proc-error` maps to `ExecutionFailed`, so each value test runs a control
// and changes only the config value under test.

/// Two seconds, then a word: outlasts a one-second budget, sits under a generous one.
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

/// `timeout-secs` from config bounds the command; same command as the control.
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

/// `output-cap` from config truncates, marked by `[truncated]`; the uncapped run is the control.
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

/// `env-passthrough` grants one name at a time, not the whole environment. Values do not overlap, so neither can satisfy the other's assertion.
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

// Denials assert the effect (the command did not run), not warning text, since a
// bug could warn and carry on.

/// `enabled: true` with no usable workspace still denies. The root must be unopenable: an absent key adopts `$PWD`.
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

/// An unreadable `execution.sandbox` denies rather than running unconfined.
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

/// Unsatisfiable `require: true` denies. Uses `approval-only`, which refuses on every platform; `mode: os` without a backend depends on the host.
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

// Confinement must come from the config boot path. The `sandbox_*.rs` tests build
// `ProcessRunner` by hand and say nothing about it. Each test runs unconfined first.

/// A directory outside any workspace, holding the escape target. Its own tree, since `run_command_in` deletes its temp tree when the turn ends.
fn escape_target(tag: &str) -> (PathBuf, common::TempDir) {
    let dir = std::env::temp_dir().join(format!("jk-execcfg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the directory outside the workspace");
    (dir.join("leak"), common::TempDir(dir))
}

/// Writes to `target` via `sh -c`, observing `wrote` on the wire: a file would be gone with the temp tree.
fn write_outside(execution: &str, target: &Path) -> ProbeTurn {
    let script = format!("echo x > {} && echo wrote", target.display());
    run_command(execution, "sh", &["-c", &script])
}

/// Control: `approval-only` wires no backend on any platform, so the escape write succeeds. Platform-independent so a machine without a backend still checks something.
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

/// A runner built from config with no `sandbox:` block is confined to the workspace.
/// Gated on a runtime backend check, since `mode: os` downgrades without one.
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

/// Confinement still permits what policy grants; otherwise the escape test passes for a runner that denies everything (#124).
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

// Long-lived children (#109): a guest's string must never become a program. Asserted
// across the Component Model boundary, since the guest is the distrusted party.

/// A config with one granted long-lived child.
const GRANTED: &str =
    "execution:\n  enabled: true\n  long-lived:\n    - name: echoer\n      command: cat\n";

/// A name the operator did not write down is refused. Its control is `a_granted_long_lived_child_starts_and_answers`.
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

/// The grant is read from config: a granted name is grantable, an unnamed one is not.
#[test]
fn the_long_lived_grant_is_read_from_config() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let policy = jk_wasm::host_process::ProcessRunner::disabled().with_long_lived(vec![
        jk_wasm::host_process::LongLived {
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

/// A guest can list the long-lived names it was granted, so a model can ask for one (#220).
#[test]
fn a_guest_can_list_the_long_lived_names_it_was_granted() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_granted(GRANTED);
    assert!(
        turn.produced("granted echoer"),
        "the granted name was not listed: {}",
        turn.tool_result
    );
}

/// The listing names children without disclosing their commands. The listing test above is its control.
#[test]
fn the_listing_names_children_without_naming_their_commands() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_granted(GRANTED);
    assert!(
        !turn.produced("cat"),
        "the command behind the name was disclosed: {}",
        turn.tool_result
    );
}

/// Granting nothing lists nothing.
#[test]
fn an_ungranting_deployment_lists_no_children() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let turn = run_granted("execution:\n  enabled: true\n");
    assert!(
        turn.produced("granted "),
        "the probe did not answer at all: {}",
        turn.tool_result
    );
    assert!(
        !turn.produced("echoer"),
        "a name nobody granted was listed: {}",
        turn.tool_result
    );
}

/// Control for the refusal test: a granted child starts, takes input, and echoes it back.
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

/// Acceptance line 3: a long-lived child is confined like a one-shot command. It outlives its call, so this is not assumed.
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

/// Acceptance line 2: a child dies with its runtime even if the guest never kills it.
/// Checked against the OS, not our own bookkeeping. The child reports its pid via
/// `echo $$`, and the runtime is already dropped when the result is read.
#[test]
fn a_long_lived_child_does_not_outlive_the_runtime() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    // `sleep`, not `cat`: `cat` exits when stdin closes, so the test would pass without a kill.
    // `exec` makes the printed pid the one that survives.
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

/// Whether a pid is still alive, asked of the OS.
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .is_ok_and(|out| out.status.success())
}
