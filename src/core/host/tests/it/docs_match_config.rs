//! Docs can drift from the shipped config with no compiler to catch it — e.g. the
//! README once told readers to `export ANTHROPIC_API_KEY` while the shipped
//! config enabled `provider.openai`, so the first command anyone ran failed at
//! boot. These tests read the shipped config and the docs and assert they agree.
//!
//! No guests staged needed: they only compare files in the repository.

use std::collections::BTreeSet;

use crate::common;

/// The `${VAR}` names the *enabled* providers in the shipped config expand.
/// Parsed rather than hard-coded, so the list can't itself drift from the config.
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
            // Flush the previous block's pending var before switching.
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
/// Matched anywhere in a line, not just at the start — the installer wraps it in
/// an `echo`, which an anchored match would miss.
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

/// A document that tells a reader to set a key must name at least the one the
/// shipped config actually asks for (it may also name alternatives).
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

/// The installer's own quick-start printout is checked the same way.
#[test]
fn the_installer_names_the_key_the_shipped_config_needs() {
    assert_names_a_required_key("scripts/install.sh");
}

/// The asset name the installer builds must be one the release workflow
/// publishes. `aarch64` (Linux) vs `arm64` (macOS) is easy to normalize wrong and
/// silently 404 for Apple Silicon users — compared as sets to catch that.
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
/// A manual audit of "which code reads this block?" missed dead blocks and an
/// unwired category before. This enumerates known-consumed keys as a whitelist —
/// adding a category or top-level key now fails until someone wires it or
/// deletes it, instead of relying on the next manual sweep to catch it.
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
    //   allow-unmanifested — Runtime::boot, the named widening for a component
    //                        that ships no manifest
    //   providers  — order_chain, applied in build_agent
    //   routing    — interceptor-task-router, via host-config
    //   registry   — ext::Checks::from_config, the trusted-keys grant for
    //                `ext install`. Distinct from `extensions.registry`, which
    //                is the category above; they share a word and nothing else.
    const CONSUMED_TOP_LEVEL: [&str; 10] = [
        "allow-unmanifested",
        "extensions",
        "workspace",
        "execution",
        "classifier",
        "providers",
        "routing",
        "storage",
        "limits",
        "registry",
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
        // `key:` and `key: value` both count — matching only bare `key:` would
        // miss scalars like `workspace: /path`.
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

    // Reverse check: a category the runtime handles but the shipped config never
    // demonstrates is a feature nobody will discover.
    for consumed in CONSUMED_CATEGORIES {
        assert!(
            categories.iter().any(|c| c == consumed),
            "`{consumed}` is instantiated but absent from the shipped config"
        );
    }
}

/// No test may skip except through the shared policy.
///
/// A skipped test reports as passing, which is fine when `JK_REQUIRE_GUESTS`
/// can turn the skip into a failure, and corrosive otherwise — silent no-op
/// tests have slipped through this way more than once. Enforced mechanically
/// instead of by eye.
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
            // Code only — matching comments too would flag prose explaining the
            // policy itself, not just an actual bare skip.
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
/// `docs/concepts/security-model.md` is a table ending in "the test that proves
/// it" for each capability. A citation that no longer resolves reads as
/// assurance and delivers none, so renaming a test breaks this until the page
/// is updated.
#[test]
fn the_security_model_cites_tests_that_exist() {
    let root = common::repo_root();
    let page = root.join("docs/concepts/security-model.md");
    let text = std::fs::read_to_string(&page).expect("the security model page is readable");

    // Citations are `<path>::<test name>`; a second test on the same file may
    // appear as a bare `::<name>`, which must inherit the previous path.
    let mut checked = 0;
    let mut last_path: Option<String> = None;
    for token in text.split('`') {
        let Some((prefix, name)) = token.split_once("::") else {
            continue;
        };
        let path = if std::path::Path::new(prefix)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        {
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

/// Every instance the shipped config declares resolves to a component that
/// exists. An *unenabled* block's component reference is otherwise tested by
/// nothing, so a stale reference (wrong `type:`, or wrong default derived from
/// the instance name) can sit unnoticed until someone flips `enabled: true`.
///
/// Uses the real parser (`jan_klod_config`), not a hand-rolled YAML scan — a
/// hand-rolled scan that only matched explicit `type:` lines previously missed
/// every block relying on the default-name derivation.
#[test]
fn every_declared_instance_resolves_to_a_component() {
    let root = common::repo_root();
    // Enabled instances expand their `${VAR}`, so parsing needs the env var set;
    // its value is irrelevant here.
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

/// Every command the docs tell a user to run exists. A documented command that
/// doesn't exist (it has happened) sends a reader straight into the boot plan
/// with no way to check first.
///
/// The changelog is excluded: it records history, including commands later
/// renamed or removed, and rewriting it to satisfy this check would be wrong.
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
                    // `make` also appears in prose ("make sure"); only treat a word
                    // as a target if the Makefile could plausibly define it.
                    let present = if verify {
                        main_rs.contains(&format!("Some(\"{word}\")"))
                    } else {
                        makefile.contains(&format!("\n{word}:"))
                            || makefile.contains(&format!("\n{word} "))
                            || makefile.contains("GUESTS := ") && makefile.contains(&word)
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

/// No shell block tells a reader to run `serve`/`verify`/`telegram` with
/// positional paths. Naming `config.yaml`/`ext` explicitly overrides the
/// resolution that finds them beside the installed binary, so a command that
/// works in a checkout fails for everyone who installed it. Only fenced shell
/// blocks are checked; prose describing the positional form is fine.
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
            // `serve` — the same footgun applies to all of them.
            for sub in ["serve", "verify", "telegram"] {
                let Some(rest) = trimmed.split_once(&format!("jan-klod-gateway {sub}")) else {
                    continue;
                };
                let first = rest.1.split_whitespace().next().unwrap_or("");
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
