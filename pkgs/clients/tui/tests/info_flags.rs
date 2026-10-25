#![allow(missing_docs)]
//! `--version`/`-V` and `--help`/`-h` against the built binary: both exit 0 without starting a gateway.
use std::process::Command;

fn run(flag: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_jan-klod"))
        .arg(flag)
        .output()
        .expect("jan-klod runs");
    assert_eq!(output.status.code(), Some(0), "jan-klod {flag}");
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

#[test]
fn version_prints_the_crate_version() {
    assert_eq!(
        run("--version"),
        format!("jan-klod {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_the_usage() {
    let help = run("--help");
    assert!(help.starts_with("Usage: jan-klod"), "{help}");
    assert!(help.contains("--addr"), "{help}");
}

#[test]
fn the_short_forms_answer_as_the_long_ones_do() {
    assert_eq!(run("-V"), run("--version"));
    assert_eq!(run("-h"), run("--help"));
}
