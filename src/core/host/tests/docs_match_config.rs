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

/// Every key in the shipped config must have code that reads it.
///
/// This is the check I kept performing by hand and got wrong. Asking "which code
/// reads this block?" found `providers:` inert, `routing:` inert and naming
/// endpoints it could not reach, and an `api-key:` advertising authentication
/// that did not exist. Then I declared the sweep complete and had missed
/// `extensions.agent`, which a two-minute grep disproved.
///
/// A confident summary is cheap and expensive when wrong, so the enumeration
/// lives here instead. Adding a category or a top-level key now fails until
/// someone either wires it or deletes it — the same shape as
/// `intercept::is_dispatched` making an unreached phase a compile error.
///
/// Deliberately a whitelist, not a parser: it does not try to prove the reader
/// exists, only that a human said which one it is. That is the part that was
/// being skipped.
#[test]
fn every_config_key_is_one_the_runtime_reads() {
    // `extensions.<category>` values the runtime instantiates.
    //   provider    — build_agent, pass 1
    //   interceptor — build_agent, pass 2
    //   registry    — build_agent, pass 1 (skills / mcp)
    //   tool        — build_agent, pass 1
    const CONSUMED_CATEGORIES: [&str; 4] =
        ["provider", "interceptor", "registry", "tool"];
    // Top-level keys, and what reads each.
    //   extensions — Config::from_path
    //   workspace  — Runtime::open_workspace
    //   execution  — Runtime::open_process_runner
    //   classifier — Runtime::open_classifier
    //   storage    — Runtime::open_store
    //   providers  — order_chain, applied in build_agent
    //   routing    — interceptor-task-router, via host-config
    const CONSUMED_TOP_LEVEL: [&str; 7] =
        ["extensions", "workspace", "execution", "classifier", "providers", "routing", "storage"];

    let config = std::fs::read_to_string(common::repo_root().join("config.yaml"))
        .expect("the shipped config.yaml is readable");

    let mut top_level = Vec::new();
    let mut categories = Vec::new();
    let mut in_extensions = false;
    for line in config.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        // `key:` and `key: value` both count. Matching only the first missed every
        // scalar — `workspace: /path` would have sailed past the check meant to
        // catch it, which is the same near-miss as the extractor that only saw
        // `export` at the start of a line.
        let Some((key, _)) = line.trim().split_once(':') else { continue };
        let key = key.trim();
        if key.is_empty() || key.contains(' ') {
            continue;
        }
        if indent == 0 {
            in_extensions = key == "extensions";
            top_level.push(key.to_string());
        } else if indent == 2 && in_extensions {
            categories.push(key.to_string());
        }
    }

    let unknown_top: Vec<&String> =
        top_level.iter().filter(|k| !CONSUMED_TOP_LEVEL.contains(&k.as_str())).collect();
    assert!(
        unknown_top.is_empty(),
        "top-level {unknown_top:?} in config.yaml — name the code that reads each, or \
         remove it. Known: {CONSUMED_TOP_LEVEL:?}"
    );

    let unknown_category: Vec<&String> =
        categories.iter().filter(|k| !CONSUMED_CATEGORIES.contains(&k.as_str())).collect();
    assert!(
        unknown_category.is_empty(),
        "extensions.{unknown_category:?} is not instantiated by anything — `build_agent` \
         assembles {CONSUMED_CATEGORIES:?}. Wire it, or make the block a note saying \
         it is inert (as `api`, `chat` and `agent` are)."
    );

    // And the reverse: a category the runtime handles but the shipped config never
    // shows is a feature nobody will find.
    for consumed in CONSUMED_CATEGORIES {
        assert!(
            categories.iter().any(|c| c == consumed),
            "`{consumed}` is instantiated but absent from the shipped config"
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
            // Code only. This matched prose as well, and tripped on a comment
            // explaining *why* something must not be skipped — a checker that
            // fires on the word rather than the act is one somebody eventually
            // silences, which costs more than the check is worth.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("///") || code.starts_with("//!") {
                continue;
            }
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

/// Every test the security model cites exists, and has that name.
///
/// `docs/concepts/security-model.md` is a table of capability → default → grant →
/// enforcement point → **the test that proves it**. That last column is the
/// reason the page is worth having: nine of its rows were written after a defect
/// in that row, and most of those defects sat in a green suite because nothing
/// asked "what proves this?".
///
/// A citation that no longer resolves is worse than no citation. It reads as
/// assurance and delivers none — the exact failure the page exists to prevent, so
/// it would be a poor joke to let the page commit it. Renaming a test now breaks
/// this until the page is updated.
#[test]
fn the_security_model_cites_tests_that_exist() {
    let root = common::repo_root();
    let page = root.join("docs/concepts/security-model.md");
    let text = std::fs::read_to_string(&page).expect("the security model page is readable");

    // Citations are written `<path>::<test name>` inside backticks, and a second
    // test on the same file as a bare `::<name>`. Those continuations have to
    // inherit the previous path: skipping them would leave several citations
    // unverified while this check reported success, which is the shape of thing
    // it exists to catch.
    let mut checked = 0;
    let mut last_path: Option<String> = None;
    for token in text.split('`') {
        let Some((prefix, name)) = token.split_once("::") else { continue };
        let path = if prefix.ends_with(".rs") {
            last_path = Some(prefix.to_string());
            prefix.to_string()
        } else if prefix.is_empty() {
            match &last_path {
                Some(previous) => previous.clone(),
                None => continue,
            }
        } else {
            continue;
        };
        let path = path.as_str();
        // Paths are relative to the two workspaces; try both roots.
        let candidates = [
            root.join("src/core").join(path),
            root.join("src/extensions").join(path),
        ];
        let found = candidates.iter().find(|p| p.exists()).unwrap_or_else(|| {
            panic!(
                "the security model cites `{path}`, which does not exist under \
                 src/core or src/extensions"
            )
        });
        let source = std::fs::read_to_string(found).expect("the cited file is readable");
        // `::a_name` for a second citation on one path also parses; take the
        // leading identifier.
        let name = name.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or(name);
        assert!(
            source.contains(&format!("fn {name}(")),
            "the security model cites `{path}::{name}`, which is not a test in that file"
        );
        checked += 1;
    }

    eprintln!("security model: {checked} citations verified");
    assert!(
        checked >= 20,
        "only {checked} citations parsed — the format changed and this check went \
         quiet, which is the failure mode it exists to prevent"
    );
}

/// Every `type:` in the shipped config resolves to a component that exists.
///
/// `provider.ollama` said `type: ollama`, which resolves to
/// `provider-ollama.wasm`. There is no such component and never was. Enabling the
/// single most common self-hosted setup therefore produced a missing component and
/// no provider at all, and the comment beside it named the file as though it
/// shipped.
///
/// This is the same failure as `extensions.store` advertising `store-postgres`:
/// a config block is a promise, and an unenabled block's promise is never tested
/// by anything — which is exactly why it needs a mechanical check rather than a
/// reader's attention. `every_config_key_is_one_the_runtime_reads` checks the
/// *keys*; this checks the values that name code.
#[test]
fn every_configured_type_names_a_component_that_exists() {
    let root = common::repo_root();
    let config = std::fs::read_to_string(root.join("config.yaml")).expect("config is readable");
    let ext = root.join("ext");

    // `<category>:` at two-space indent, then `type: <name>` deeper in.
    let mut category = String::new();
    let mut checked = Vec::new();
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 2 {
            if let Some(name) = trimmed.strip_suffix(':') {
                category = name.to_string();
            }
        }
        let Some((key, value)) = trimmed.split_once(": ") else { continue };
        if key != "type" {
            continue;
        }
        // Strip a trailing comment.
        let kind = value.split('#').next().unwrap_or(value).trim();
        if kind.is_empty() || category.is_empty() {
            continue;
        }
        let file = format!("{category}-{kind}.wasm");
        assert!(
            ext.join(&file).exists(),
            "config.yaml names `type: {kind}` under `{category}`, which resolves to \
             {file} — and no such component exists. Enabling that block yields a \
             missing component and no {category} at all."
        );
        checked.push(file);
    }

    assert!(
        checked.len() >= 4,
        "only {} types parsed, so this check went quiet — the config format changed",
        checked.len()
    );
}
