//! The `self-extend` distribution compiles a real toolchain in its sandbox, and the default one refuses. Both halves are needed.
//! The `execution:` block is spliced from the shipped config, so this tests what ships.

use jk_agent::Runtime;

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

/// The shipped `execution:` block, spliced verbatim to the end of the file.
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

/// The install step exists in the booted fleet (#221). Checked against the fleet,
/// not the YAML: a `registry:` key nobody reads is what this failed as.
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

/// `cargo build` as a tool call would make it. `--offline`: the distribution denies
/// the network, so a build reaching for it would hang.
const BUILD_ARGS: [&str; 4] = ["build", "--offline", "--target", "wasm32-wasip2"];

/// A seeded workspace under a test-controlled path. `None` when cargo is absent.
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

/// `wasm32-wasip2` is not checked separately: `make extensions` needs it to stage these.
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

/// The compiling distribution still refuses a write outside the workspace.
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

/// The same call under `coding`, which has execution disabled. This makes the
/// test above about the distribution, not cargo.
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
    assert!(
        turn.produced("execution-failed"),
        "and the model was told the call failed rather than being handed an \
         empty success: {}",
        turn.tool_result
    );
}
