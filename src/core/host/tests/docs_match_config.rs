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
            if let Some(var) = rest
                .trim()
                .strip_prefix("${")
                .and_then(|v| v.strip_suffix('}'))
            {
                pending_var = Some(var.to_string());
            }
        }
    }
    if in_providers && instance_enabled {
        vars.extend(pending_var);
    }

    assert!(
        !vars.is_empty(),
        "the shipped config enables a provider with an api-key"
    );
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
    let workflow =
        std::fs::read_to_string(common::repo_root().join(".github/workflows/release.yml"))
            .expect("the release workflow is readable");
    let installer = std::fs::read_to_string(common::repo_root().join("scripts/install.sh"))
        .expect("install.sh is readable");

    let published: BTreeSet<String> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("arch:"))
        .map(|a| a.trim().to_string())
        .collect();
    assert!(
        !published.is_empty(),
        "the workflow names the architectures it builds"
    );

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
    const CONSUMED_CATEGORIES: [&str; 4] = ["provider", "interceptor", "registry", "tool"];
    // Top-level keys, and what reads each.
    //   extensions — Config::from_path
    //   workspace  — Runtime::open_workspace
    //   execution  — Runtime::open_process_runner
    //   classifier — Runtime::open_classifier
    //   storage    — Runtime::open_store
    //   limits     — Runtime::limits
    //   providers  — order_chain, applied in build_agent
    //   routing    — interceptor-task-router, via host-config
    const CONSUMED_TOP_LEVEL: [&str; 8] = [
        "extensions",
        "workspace",
        "execution",
        "classifier",
        "providers",
        "routing",
        "storage",
        "limits",
    ];

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
        let Some((key, _)) = line.trim().split_once(':') else {
            continue;
        };
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

    let unknown_top: Vec<&String> = top_level
        .iter()
        .filter(|k| !CONSUMED_TOP_LEVEL.contains(&k.as_str()))
        .collect();
    assert!(
        unknown_top.is_empty(),
        "top-level {unknown_top:?} in config.yaml — name the code that reads each, or \
         remove it. Known: {CONSUMED_TOP_LEVEL:?}"
    );

    let unknown_category: Vec<&String> = categories
        .iter()
        .filter(|k| !CONSUMED_CATEGORIES.contains(&k.as_str()))
        .collect();
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

    for entry in std::fs::read_dir(&tests)
        .expect("the test directory is readable")
        .flatten()
    {
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
                let file = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
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
        let Some((prefix, name)) = token.split_once("::") else {
            continue;
        };
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
        let name = name
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or(name);
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

/// Every instance the shipped config declares resolves to a component that exists.
///
/// `provider.ollama` said `type: ollama` → `provider-ollama.wasm`, which has never
/// existed, so enabling the most common self-hosted setup produced a missing
/// component and no provider. `tool.web-search` had no `type:` at all, which
/// defaults to the instance name → `tool-web-search.wasm`, equally absent.
///
/// A config block is a promise, and an *unenabled* block's promise is tested by
/// nothing — which is exactly why it needs a mechanical check rather than a
/// reader's attention. The same failure retired `extensions.store`'s
/// `store-postgres` and the inert `agent`/`api`/`chat` categories.
///
/// This asks the **real parser**, not a hand-rolled scan of the YAML. The first
/// version read indentation itself and matched only explicit `type:` lines, so it
/// missed every block relying on the default — thirteen of the fifteen, including
/// the one that was broken. `jan_klod_config` owns the `<category>-<kind>`
/// derivation the runtime resolves, so asking it is both shorter and incapable of
/// drifting from the thing under test.
#[test]
fn every_declared_instance_resolves_to_a_component() {
    let root = common::repo_root();
    // Enabled instances get their `${VAR}` expanded, so the parse needs the key
    // the shipped config asks for. Its value is irrelevant here.
    std::env::set_var("OPENAI_API_KEY", "placeholder-for-parsing");
    let config = jan_klod_config::Config::from_path(root.join("config.yaml"))
        .expect("the shipped config parses");
    let ext = root.join("ext");

    let missing: Vec<String> = config
        .instances
        .iter()
        .filter(|instance| !ext.join(instance.component_file()).exists())
        .map(|instance| format!("{} -> {}", instance.id, instance.component_file()))
        .collect();

    assert!(
        missing.is_empty(),
        "these config blocks name components that do not exist: {missing:?}. A block \
         that cannot load is not a placeholder, it is a trap for whoever flips \
         `enabled: true` — build the component, or comment the block out with a note \
         saying what it would take."
    );
    assert!(
        config.instances.len() >= 10,
        "only {} instances parsed, so this check went quiet",
        config.instances.len()
    );
}

/// Every command the docs tell a user to run exists.
///
/// `jan-klod-gateway ask` was documented in two places before it existed. I wrote
/// those sentences myself, to justify withholding stdin from guests — the fix was
/// right, the example was invented — and then half-believed the subcommand two
/// days later and had to go and check. A reader has no way to check; they type it
/// and get a boot plan.
///
/// The changelog is excluded on purpose. It is a record of what happened, not
/// instructions, and it necessarily contains commands that were later renamed or
/// removed. Rewriting history to satisfy a linter would be the wrong repair.
#[test]
fn every_documented_command_exists() {
    let root = common::repo_root();
    let main_rs = std::fs::read_to_string(root.join("src/core/host/src/main.rs"))
        .expect("the gateway's main is readable");
    let makefile = std::fs::read_to_string(root.join("Makefile")).expect("Makefile is readable");

    let mut pages = Vec::new();
    for dir in ["docs", "docs/concepts", "docs/guides"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && path.file_name().is_some_and(|n| n != "changelog.md")
            {
                pages.push(path);
            }
        }
    }
    pages.push(root.join("README.md"));

    let mut checked = 0;
    for page in &pages {
        let Ok(text) = std::fs::read_to_string(page) else {
            continue;
        };
        let name = page
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        for (line_no, line) in text.lines().enumerate() {
            for (prefix, verify) in [("jan-klod-gateway ", true), ("make ", false)] {
                let mut rest = line;
                while let Some(at) = rest.find(prefix) {
                    rest = &rest[at + prefix.len()..];
                    let word: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                        .collect();
                    if word.is_empty() {
                        continue;
                    }
                    // `make` appears in prose ("make sure", "make it"); only treat
                    // a word as a target if the Makefile could plausibly define it.
                    let present = if verify {
                        main_rs.contains(&format!("Some(\"{word}\")"))
                    } else {
                        makefile.contains(&format!("\n{word}:"))
                            || makefile.contains(&format!("\n{word} "))
                            || makefile.contains(&format!("GUESTS := ")) && makefile.contains(&word)
                    };
                    if verify {
                        assert!(
                            present,
                            "{name}:{} tells the reader to run `jan-klod-gateway {word}`, \
                             which the gateway does not handle — they get the boot plan",
                            line_no + 1
                        );
                        checked += 1;
                    } else if present {
                        checked += 1;
                    }
                }
            }
        }
    }

    assert!(
        checked >= 5,
        "only {checked} commands parsed — this check went quiet"
    );
}

/// No shell block tells a reader to run `serve` with positional paths.
///
/// The README recommended `jan-klod-gateway serve config.yaml ext`, which works in
/// a checkout and fails for everyone who installed the binary — the resolution
/// that finds `config.yaml` beside the executable is *overridden* by naming it, so
/// the recommended command was the one that breaks in the common case. It also
/// invited `serve 127.0.0.1:8787`, read as a config path.
///
/// Only fenced shell blocks are checked. Prose explaining that the positional form
/// exists is useful and stays; a command a reader will copy is different.
#[test]
fn no_shell_block_recommends_positional_serve_paths() {
    let root = common::repo_root();
    let mut pages = vec![root.join("README.md")];
    for dir in ["docs", "docs/concepts", "docs/guides"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && path.file_name().is_some_and(|n| n != "changelog.md")
            {
                pages.push(path);
            }
        }
    }

    for page in &pages {
        let Ok(text) = std::fs::read_to_string(page) else {
            continue;
        };
        let name = page
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut in_shell = false;
        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                in_shell = trimmed.contains("sh") || trimmed.contains("bash");
                continue;
            }
            if !in_shell {
                continue;
            }
            // Every subcommand that accepts `[config] [ext]` positionally, not just
            // `serve`. The first version of this check named one, and the quickstart
            // was showing `verify config.yaml ext` two screens further down — the
            // same footgun, missed because the lint was written around the instance
            // that prompted it.
            for sub in ["serve", "verify", "telegram"] {
                let Some(rest) = trimmed.split_once(&format!("jan-klod-gateway {sub}")) else {
                    continue;
                };
                let first = rest.1.trim().split_whitespace().next().unwrap_or("");
                assert!(
                    first.is_empty() || first.starts_with("--"),
                    "{name}:{} shows `jan-klod-gateway {sub} {first}` — a positional path \
                     overrides the resolution that finds config.yaml beside the installed \
                     binary, so this command works in a checkout and fails for everyone \
                     who installed it",
                    line_no + 1
                );
            }
        }
    }
}
