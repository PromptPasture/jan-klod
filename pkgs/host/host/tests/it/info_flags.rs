//! `jan-klod-gateway --version`/`-V` and `--help`/`-h` answer and exit 0 instead of falling through to the boot plan.

use std::process::Command;

fn run(flag: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg(flag)
        .output()
        .expect("the gateway runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "jan-klod-gateway {flag}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

#[test]
fn version_prints_the_crate_version() {
    assert_eq!(
        run("--version"),
        format!("jan-klod-gateway {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_lists_every_subcommand() {
    let help = run("--help");
    for command in [
        "serve", "rpc", "telegram", "setup", "verify", "ask", "ext", "mcp", "acp",
    ] {
        assert!(
            help.contains(&format!("\n  {command}")),
            "missing `{command}`:\n{help}"
        );
    }
}

#[test]
fn the_short_forms_answer_as_the_long_ones_do() {
    assert_eq!(run("-V"), run("--version"));
    assert_eq!(run("-h"), run("--help"));
}
