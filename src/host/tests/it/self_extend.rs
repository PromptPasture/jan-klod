//! The `self-extend` distribution compiles, and the default one refuses.
//!
//! Slice 25a's acceptance, and the pair matters more than either half: a test
//! that only showed a build succeeding would pass against a runtime with no
//! sandbox at all, and a test that only showed a refusal would pass against
//! one where nothing works.
//!
//! # What this asserts that the sandbox tests do not
//!
//! `sandbox_seatbelt.rs` and `sandbox_landlock.rs` prove the boundary holds
//! for a single `sh -c`. This proves a *real toolchain* runs inside it —
//! `cargo` spawning `rustc`, writing a target directory, using `TMPDIR`,
//! redirecting to `/dev/null` somewhere in its own plumbing — from the
//! execution block the distribution actually ships.
//!
//! # The execution block is read, not restated
//!
//! Both tests splice the `execution:` block out of the distribution's own
//! `config.yaml`. A copy written here would keep passing after someone
//! changed the shipped file, which is the failure this slice is most exposed
//! to: the whole claim is about what a distribution is configured to permit.
//!
//! # What it deliberately does not build
//!
//! A dependency-free crate, not a real guest. The 47-crate vendored build was
//! verified offline by hand for #192 and takes ~15 s; repeating it here would
//! put that on every `make gate` to test the same boundary this does in about
//! a second. What is under test is the sandbox, not `wit-bindgen`.

use jan_klod_core::Runtime;

use crate::common;
use crate::execution_config::run_command_in;

/// The guests `self-extend` needs before any of this means anything.
pub const NEEDED: &[&str] = &[
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-fs.wasm",
    "tool-edit.wasm",
    "tool-find.wasm",
    "tool-git.wasm",
    "tool-shell.wasm",
];

/// The shipped distribution, pointed at a mock provider and a temp directory.
fn shipped_config(dir: &std::path::Path) -> String {
    let path = common::repo_root().join("scripts/distributions/self-extend/config.yaml");
    let mut config = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} is readable: {err}", path.display()));
    for (from, to) in [
        (
            "base-url: https://api.openai.com/v1",
            "base-url: http://mock/v1",
        ),
        ("api-key: ${OPENAI_API_KEY}", "api-key: test"),
        ("path: ./jan-klod.db", "path: ./self-extend-measure.db"),
    ] {
        assert!(
            config.contains(from),
            "{} no longer contains `{from}`, so this test is measuring a \
             distribution nobody ships",
            path.display()
        );
        config = config.replace(from, to);
    }
    format!("{config}\nworkspace: {}\n", dir.display())
}

/// Every tool `self-extend` advertises, as the interceptor is served them.
pub fn shipped_fleet() -> Option<(common::TempDir, serde_json::Value)> {
    if !common::guests_staged(NEEDED) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-toolsel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the workspace");
    let guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(&config, shipped_config(&dir)).expect("writes the config");

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime
        .build_agent(&|| common::canned_http("ok"))
        .expect("the distribution boots");
    let metas = agent.all_metas_json().expect("the fleet reports its tools");
    Some((guard, metas))
}

/// The `execution:` block the distribution ships, spliced verbatim.
///
/// It is the last block in each of these files, so "from the line to the end"
/// is the whole of it. Guarded rather than trusted: if the file is reordered
/// this returns something that is not an execution block, and the assertion
/// below says so instead of silently testing a config nobody ships.
pub fn shipped_execution_block(distribution: &str) -> String {
    let path = common::repo_root()
        .join("scripts/distributions")
        .join(distribution)
        .join("config.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{} is readable: {err}", path.display()));
    let start = text
        .find("\nexecution:\n")
        .unwrap_or_else(|| panic!("{} has no top-level `execution:` block", path.display()));
    let block = text[start + 1..].to_owned();
    assert!(
        block.contains("sandbox:"),
        "the `execution:` block in {} is not the last block in the file, so \
         splicing to the end no longer yields it: {block}",
        path.display()
    );
    block
}

/// The install step of the chain exists at all (#221).
///
/// `registry.install-tool` is what decides whether `ext-install` is in the
/// fleet, and this distribution left it unset until now — so
/// compile → install → load → call had no install step, in the one
/// distribution that slice is about. Asserted against the **booted fleet**
/// rather than the YAML, because a key read by nobody is what this failed
/// as: the config said `registry:` and the model saw five tools.
#[test]
fn the_distribution_offers_the_agent_a_way_to_install_what_it_built() {
    let Some((_guard, tools)) = shipped_fleet() else {
        return;
    };
    let names: Vec<&str> = tools
        .as_array()
        .expect("an array of tools")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .collect();
    assert!(
        names.contains(&"ext-install"),
        "the agent cannot install what it compiles: {names:?}"
    );
    // Its control: the fleet is the real one, not an empty list that would
    // make any absence assertion pass.
    assert!(
        names.contains(&"shell"),
        "the compiling half of the chain is missing too: {names:?}"
    );
}

/// A crate that compiles to wasm and needs nothing from the network.
fn seed_crate(workspace: &std::path::Path) {
    std::fs::create_dir_all(workspace.join("src")).expect("creates the crate");
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"probe-guest\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n",
    )
    .expect("writes the manifest");
    std::fs::write(
        workspace.join("src/lib.rs"),
        "#[unsafe(no_mangle)]\npub extern \"C\" fn answer() -> i32 {\n    42\n}\n",
    )
    .expect("writes the source");
}

/// `cargo build`, as a tool call would make it. `--offline` because the
/// distribution denies the network and a build that reached for it would hang
/// rather than fail cleanly.
const BUILD_ARGS: [&str; 4] = ["build", "--offline", "--target", "wasm32-wasip2"];

/// A workspace under a path the test controls, seeded and returned with its
/// guard. `None` when the toolchain is absent.
fn workspace(tag: &str) -> Option<(common::TempDir, std::path::PathBuf)> {
    if !common::tool_available("cargo") {
        eprintln!("skipping: cargo is not on PATH");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-selfext-{tag}-{}", std::process::id()));
    let ws = dir.join("workspace");
    std::fs::create_dir_all(&ws).expect("creates the workspace");
    seed_crate(&ws);
    Some((common::TempDir(dir), ws))
}

/// The `wasm32-wasip2` target is not checked for separately: `guests_staged`
/// below is true only when `make extensions` has built the Rust guests, which
/// needs that target. A machine that can reach this line has it.
const GUESTS: [&str; 2] = ["provider-openai.wasm", "tool-proc-probe.wasm"];

#[test]
fn the_self_extend_distribution_compiles_to_wasm_inside_the_jail() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let Some((_guard, ws)) = workspace("build") else {
        return;
    };

    let turn = run_command_in(
        Some(&ws.display().to_string()),
        &shipped_execution_block("self-extend"),
        "cargo",
        &BUILD_ARGS,
    );

    let artifact = ws.join("target/wasm32-wasip2/debug/probe_guest.wasm");
    assert!(
        artifact.exists(),
        "the compiler ran to completion inside the workspace and left its \
         artifact: {}\nthe tool reported: {}",
        artifact.display(),
        turn.tool_result
    );
    assert!(
        turn.completions >= 2,
        "and the result reached the model: {turn:?}",
        turn = turn.completions
    );
}

/// Compiling did not come at the boundary's expense.
///
/// The test above is already load-bearing here — `require: true` means a
/// platform that resolved no backend denies every command, so an artifact
/// existing proves something confined the build. This says the confinement
/// still refuses: same distribution, same runner, a write one directory up.
#[test]
fn the_self_extend_distribution_still_refuses_a_write_outside_the_workspace() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let Some((guard, ws)) = workspace("escape") else {
        return;
    };
    let outside = guard.0.join("outside");
    std::fs::create_dir_all(&outside).expect("creates the sibling");
    let target = outside.join("leak");

    let turn = run_command_in(
        Some(&ws.display().to_string()),
        &shipped_execution_block("self-extend"),
        "/bin/sh",
        &["-c", &format!("echo x > {}", target.display())],
    );

    assert!(
        !target.exists(),
        "a command under the compiling distribution wrote outside the \
         workspace: {}\nthe tool reported: {}",
        target.display(),
        turn.tool_result
    );
}

/// The same call, under the distribution that does not ship a shell.
///
/// This is what makes the test above say something about the *distribution*
/// rather than about cargo. `coding`'s `execution.enabled` is `false`, so the
/// runner denies before any sandbox question arises — which is the posture
/// this slice was required not to change.
#[test]
fn the_coding_distribution_refuses_the_same_command() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let Some((_guard, ws)) = workspace("refused") else {
        return;
    };

    let turn = run_command_in(
        Some(&ws.display().to_string()),
        &shipped_execution_block("coding"),
        "cargo",
        &BUILD_ARGS,
    );

    assert!(
        !ws.join("target").exists(),
        "no build happened: cargo never ran, so it created no target directory"
    );
    // The model is told `execution-failed`, and that is all: the reason
    // ("Execution is disabled, or the `cwd` escapes the workspace") stays in
    // the host's log, because `tool-proc-probe` does not carry `ProcError`'s
    // message into its `ToolError`. That is this probe's shape rather than
    // the product's, so it is asserted as it is rather than wished otherwise
    // — the claim here is that the call failed and nothing was built.
    assert!(
        turn.produced("execution-failed"),
        "and the model was told the call failed rather than being handed an \
         empty success: {}",
        turn.tool_result
    );
}
