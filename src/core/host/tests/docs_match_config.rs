//! The first five minutes have to work.
//!
//! The README told a new user to `export ANTHROPIC_API_KEY` and run. The shipped
//! `config.yaml` enables `provider.openai`, so following it verbatim produced
//! `boot failed: provider.openai: environment variable OPENAI_API_KEY is not set`
//! — the first thing anyone does with this project, broken, in the most visible
//! file in the repository.
//!
//! It is the same defect as the rest of this suite guards against — a claim the
//! code does not honour — but in prose, where no compiler looks. So it is checked
//! the same way: read what the shipped config actually requires, then read what
//! the docs actually say, and assert they are the same thing.
//!
//! These tests need no guests staged: they compare two files in the repository.

use std::collections::BTreeSet;

mod common;

/// The `${VAR}` names the *enabled* providers in the shipped config expand.
///
/// Parsed rather than hard-coded: a hard-coded list is one more thing to drift,
/// which is precisely the failure being tested for.
fn required_key_vars() -> BTreeSet<String> {
    let config = std::fs::read_to_string(common::repo_root().join("config.yaml"))
        .expect("the shipped config.yaml is readable");

    let mut vars = BTreeSet::new();
    let mut in_providers = false;
    let mut instance_enabled = false;
    let mut pending_var: Option<String> = None;

    for line in config.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        // `  provider:` opens the block; any other 2-space key closes it.
        if indent == 2 && trimmed.ends_with(':') {
            // Flush whatever the previous block left pending.
            if in_providers && instance_enabled {
                vars.extend(pending_var.take());
            }
            in_providers = trimmed == "provider:";
            instance_enabled = false;
            pending_var = None;
            continue;
        }
        if !in_providers {
            continue;
        }
        // `    <name>:` starts a new instance — bank the previous one first.
        if indent == 4 && trimmed.ends_with(':') {
            if instance_enabled {
                vars.extend(pending_var.take());
            }
            instance_enabled = false;
            pending_var = None;
            continue;
        }
        if trimmed.starts_with("enabled:") {
            instance_enabled = trimmed.contains("true");
        }
        if let Some(rest) = trimmed.strip_prefix("api-key:") {
            if let Some(var) = rest.trim().strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
                pending_var = Some(var.to_string());
            }
        }
    }
    if in_providers && instance_enabled {
        vars.extend(pending_var);
    }

    assert!(!vars.is_empty(), "the shipped config enables a provider with an api-key");
    vars
}

/// Every `export SOMETHING=` a document tells the reader to run.
///
/// Found anywhere in a line, not just at its start: the README puts it in a code
/// fence, the installer inside an `echo`. Anchoring to the start silently skipped
/// the installer, which is the sort of near-miss that makes a check reassuring
/// rather than useful.
fn exported_vars(doc: &str) -> BTreeSet<String> {
    let text = std::fs::read_to_string(common::repo_root().join(doc))
        .unwrap_or_else(|err| panic!("{doc} is readable: {err}"));
    let mut vars = BTreeSet::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("export ") {
            rest = &rest[at + "export ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                vars.insert(name);
            }
        }
    }
    vars
}

/// A document that tells a reader to set a key must name the one the shipped
/// config will actually ask for.
///
/// Not "must name only that one" — the README goes on to describe switching to
/// Anthropic, and a doc that explains alternatives is better than one that does
/// not. What it may not do is send the reader to a key the default config never
/// reads, which is what happened.
fn assert_names_a_required_key(doc: &str) {
    let required = required_key_vars();
    let exported = exported_vars(doc);
    assert!(
        !exported.is_empty(),
        "{doc} tells the reader to export something (it is the getting-started path)"
    );
    assert!(
        exported.iter().any(|var| required.contains(var)),
        "{doc} exports {exported:?}, but the shipped config needs one of {required:?} — \
         following it verbatim would fail at boot"
    );
}

#[test]
fn the_readme_names_the_key_the_shipped_config_needs() {
    assert_names_a_required_key("README.md");
}

#[test]
fn the_landing_page_names_the_key_the_shipped_config_needs() {
    assert_names_a_required_key("pages/index.md");
}

#[test]
fn the_quickstart_names_the_key_the_shipped_config_needs() {
    assert_names_a_required_key("docs/quickstart.md");
}

/// The installer prints a quick start too, and it drifted the same way.
#[test]
fn the_installer_names_the_key_the_shipped_config_needs() {
    assert_names_a_required_key("scripts/install.sh");
}

/// The asset name the installer builds must be one the release workflow
/// publishes.
///
/// They spell 64-bit ARM differently per OS — `aarch64` on Linux, `arm64` on
/// macOS — and the installer normalised both to `aarch64`, so it asked for a file
/// that is never built: a 404 on every Apple Silicon Mac, which is most people
/// who would try it. Compared as sets rather than by reading either file.
#[test]
fn the_installer_asks_for_arch_names_the_release_actually_builds() {
    let workflow = std::fs::read_to_string(
        common::repo_root().join(".github/workflows/release.yml"),
    )
    .expect("the release workflow is readable");
    let installer = std::fs::read_to_string(common::repo_root().join("scripts/install.sh"))
        .expect("install.sh is readable");

    let published: BTreeSet<String> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("arch:"))
        .map(|a| a.trim().to_string())
        .collect();
    assert!(!published.is_empty(), "the workflow names the architectures it builds");

    for arch in &published {
        assert!(
            installer.contains(arch.as_str()),
            "install.sh never mentions `{arch}`, which the release publishes —              a user on it would get a 404. Published: {published:?}"
        );
    }
}

/// No test may skip except through the shared policy.
///
/// A skipped test reports as passing. That is tolerable when the skip is
/// governed — `JK_REQUIRE_GUESTS` turns it into a failure wherever the
/// prerequisite is meant to be present — and corrosive otherwise, because the
/// test occupies the space where a real check would be while proving nothing.
///
/// This repository learned that three times in a week: a broken assertion sat
/// green in `shipped_defaults` because `ext/` was empty; a new suite reported
/// three passes in 0.00 s on its first run for the same reason; and the
/// pre-commit hook deleted `ext/` before testing, so roughly thirty component
/// tests were no-ops on every commit while it printed success. Each was found by
/// eye. This one is found by the suite.
#[test]
fn every_test_that_skips_does_so_through_the_shared_policy() {
    let tests = common::repo_root().join("src/core/host/tests");
    let mut offenders = Vec::new();

    for entry in std::fs::read_dir(&tests).expect("the test directory is readable").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("a test file is readable");
        for (line_no, line) in source.lines().enumerate() {
            // The policy helpers own the word; anywhere else it is a bare skip.
            if line.contains("skipping") && !line.contains("common::") {
                let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                offenders.push(format!("{file}:{}: {}", line_no + 1, line.trim()));
            }
        }
    }
    // `common/mod.rs` is where the policy lives, so its own messages are the point.
    offenders.retain(|o| !o.starts_with("mod.rs"));

    assert!(
        offenders.is_empty(),
        "these skip without going through `common::guests_staged` / \
         `common::tool_available`, so they cannot be made to fail:\n  {}",
        offenders.join("\n  ")
    );
}

/// The parser has to actually distinguish enabled from disabled, or the checks
/// above pass for the wrong reason.
#[test]
fn only_enabled_providers_count_as_required() {
    let required = required_key_vars();
    assert!(
        required.contains("OPENAI_API_KEY"),
        "the enabled provider's key is required: {required:?}"
    );
    assert!(
        !required.contains("ANTHROPIC_API_KEY"),
        "a disabled provider's key is not required: {required:?}"
    );
    assert!(
        !required.contains("SEARCH_API_KEY"),
        "only the provider block is read, not every api-key in the file: {required:?}"
    );
}
