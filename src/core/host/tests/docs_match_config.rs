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
fn exported_vars(doc: &str) -> BTreeSet<String> {
    let text = std::fs::read_to_string(common::repo_root().join(doc))
        .unwrap_or_else(|err| panic!("{doc} is readable: {err}"));
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("export "))
        .filter_map(|rest| rest.split('=').next())
        .map(str::to_string)
        .collect()
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
