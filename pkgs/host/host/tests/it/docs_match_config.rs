//! Docs can drift from the shipped config with no compiler to catch it; these tests compare the two.
//! Needs no staged guests: it only reads repository files.

use std::collections::BTreeSet;

use crate::common;

/// Every shipped config (root and per distribution) as `(label, contents)`, so a failure names the config.
fn shipped_configs() -> Vec<(String, String)> {
    let root = common::repo_root();
    let mut out = vec![(
        "config.yaml".to_owned(),
        std::fs::read_to_string(root.join("config.yaml"))
            .expect("the shipped config.yaml is readable"),
    )];
    let dists = root.join("scripts/distributions");
    let mut names: Vec<_> = std::fs::read_dir(&dists)
        .expect("scripts/distributions is readable")
        .flatten()
        .filter(|e| e.path().join("config.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let path = dists.join(&name).join("config.yaml");
        out.push((
            format!("scripts/distributions/{name}/config.yaml"),
            std::fs::read_to_string(&path).expect("a distribution config is readable"),
        ));
    }
    out
}

/// The `${VAR}` names the *enabled* providers in one config expand.
/// Parsed so the list can't drift from the config.
fn required_key_vars_in(config: &str) -> BTreeSet<String> {
    let config = config.to_owned();

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
/// Matched anywhere in a line — the installer wraps it in an `echo`.
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
/// shipped config asks for (it may name alternatives too).
fn assert_names_a_required_key(doc: &str) {
    let exported = exported_vars(doc);
    assert!(
        !exported.is_empty(),
        "{doc} tells the reader to export something (it is the getting-started path)"
    );
    // Checked per config, not pooled: pooling would let one provider's key satisfy every config (#114, #63).
    for (label, config) in shipped_configs() {
        let required = required_key_vars_in(&config);
        if required.is_empty() {
            continue; // a config with no enabled provider asks for no key
        }
        assert!(
            exported.iter().any(|var| required.contains(var)),
            "{doc} exports {exported:?}, but {label} needs one of {required:?} — \
             following it verbatim would fail at boot for whoever installed that one"
        );
    }
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

/// The installer's arch names must match what the release publishes; `aarch64` vs `arm64` is an easy 404.
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

/// Distributions defined, offered by the installer, and built by the release must be one set; a mismatch is a 404 for the user.
#[test]
fn the_installer_offers_the_distributions_the_release_builds() {
    let root = common::repo_root();

    let defined: BTreeSet<String> = std::fs::read_dir(root.join("scripts/distributions"))
        .expect("scripts/distributions is readable")
        .flatten()
        .filter(|e| e.path().join("config.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !defined.is_empty(),
        "no distributions defined — this check would pass by comparing three \
         empty sets"
    );

    let installer =
        std::fs::read_to_string(root.join("scripts/install.sh")).expect("install.sh is readable");
    let offered: BTreeSet<String> = installer
        .lines()
        .find_map(|l| l.trim().strip_prefix("DISTRIBUTIONS="))
        .map(|list| {
            list.trim_matches('"')
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .expect("install.sh names the distributions it offers in DISTRIBUTIONS=");

    let workflow = std::fs::read_to_string(root.join(".github/workflows/release.yml"))
        .expect("the release workflow is readable");
    // Only the distribution name: anything after it on the line is not part of
    // the name, and must not read as a distribution nothing defines.
    let built: BTreeSet<String> = workflow
        .lines()
        .filter_map(|l| l.trim().strip_prefix("make bundle DIST="))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .collect();

    assert_eq!(
        offered, defined,
        "install.sh offers {offered:?} but scripts/distributions/ defines \
         {defined:?} — a name the installer accepts that nothing defines cannot \
         be built"
    );
    assert_eq!(
        built, defined,
        "release.yml builds {built:?} but scripts/distributions/ defines \
         {defined:?} — a distribution nobody publishes is a 404 for whoever \
         asks the installer for it"
    );
}

/// Every key in the shipped config must be read by code. The whitelist makes a new key fail until it is wired or removed.
#[test]
fn every_config_key_is_one_the_runtime_reads() {
    // `extensions.<category>` values the runtime instantiates.
    //   provider    — build_agent, pass 1
    //   interceptor — build_agent, pass 2
    //   registry    — build_agent, pass 1 (skills / mcp)
    //   tool        — build_agent, pass 1
    // The runtime's own list, not a copy (#222: a category nothing dispatches once slipped through).
    const CONSUMED_CATEGORIES: [&str; 4] = jk_wasm::CATEGORIES;
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
    //                `ext install`, and ext_index::url_from_config, the index
    //                `ext search` reads. Distinct from `extensions.registry`,
    //                which is the category above; they share a word and
    //                nothing else.
    //   observability — Config::from_path, the output-format setting that the
    //                   tracing subscriber reads at boot
    const CONSUMED_TOP_LEVEL: [&str; 11] = [
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
        "observability",
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
        // Matches `key:` and `key: value`; bare-key matching would miss scalars.
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

    // Reverse check: a category the runtime handles but the config never shows
    // is a feature nobody will discover.
    for consumed in CONSUMED_CATEGORIES {
        assert!(
            categories.iter().any(|c| c == consumed),
            "`{consumed}` is instantiated but absent from the shipped config"
        );
    }
}

/// No test may skip except through the shared policy, so `JK_REQUIRE_GUESTS` can turn skips into failures.
#[test]
fn every_test_that_skips_does_so_through_the_shared_policy() {
    let tests = common::repo_root().join("pkgs/host/host/tests");
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
            // Code only — skip comments to avoid flagging prose explaining the policy.
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with("///") || code.starts_with("//!") {
                continue;
            }
            // Policy helpers own the word; anywhere else it is a bare skip.
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
    // `common/mod.rs` is where the policy lives, so exclude its messages.
    offenders.retain(|o| !o.starts_with("mod.rs"));

    assert!(
        offenders.is_empty(),
        "these skip without going through `common::guests_staged` / \
         `common::tool_available`, so they cannot be made to fail:\n  {}",
        offenders.join("\n  ")
    );
}

/// The parser must distinguish enabled from disabled, or the checks above pass for the wrong reason.
#[test]
fn only_enabled_providers_count_as_required() {
    // The root config has a disabled provider, which lets this tell the two apart.
    let root = std::fs::read_to_string(common::repo_root().join("config.yaml"))
        .expect("the shipped config.yaml is readable");
    let required = required_key_vars_in(&root);
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

/// Every test cited by `docs/concepts/security-model.md` exists under that name. A renamed test breaks this until the page is updated.
#[test]
fn the_security_model_cites_tests_that_exist() {
    let root = common::repo_root();
    let page = root.join("docs/concepts/security-model.md");
    let text = std::fs::read_to_string(&page).expect("the security model page is readable");

    // A bare `::<name>` citation inherits the previous path.
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
        // Relative to a workspace root, falling back to the repository root.
        let candidates = [
            root.join("pkgs/host").join(path),
            root.join("pkgs/extensions").join(path),
            root.join(path),
        ];
        let found = candidates.iter().find(|p| p.exists()).unwrap_or_else(|| {
            panic!(
                "the security model cites `{path}`, which does not exist under \
                 pkgs/host, pkgs/extensions, or the repository root"
            )
        });
        let source = std::fs::read_to_string(found).expect("the cited file is readable");
        // Take the leading identifier, so `::a_name` suffixes parse.
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

/// Every wire command in `jk_protocol::COMMAND_METHODS` has a row in the Commands table in `docs/concepts/contracts.md`.
/// `pkgs/host/protocol/tests/wire.rs` checks the code side; this guards the hand-written table.
#[test]
fn the_commands_table_lists_every_wire_command() {
    let root = common::repo_root();
    let page = root.join("docs/concepts/contracts.md");
    let text = std::fs::read_to_string(&page).expect("the contracts page is readable");

    let missing: Vec<&str> = jk_protocol::COMMAND_METHODS
        .iter()
        .copied()
        .filter(|method| !text.contains(&format!("`{method}`")))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/concepts/contracts.md's Commands table is missing {missing:?} — every name in \
         `jk_protocol::COMMAND_METHODS` must appear as `` `method` `` somewhere on the \
         page, or a non-Rust client reading only the docs sees fewer commands than the schema \
         ships"
    );
}

/// Every declared instance, including unenabled ones, resolves to an existing component. Uses the real parser, not hand-rolled YAML.
#[test]
fn every_declared_instance_resolves_to_a_component() {
    let root = common::repo_root();
    // Parsing expands enabled instances' `${VAR}`; the value does not matter.
    std::env::set_var("OPENAI_API_KEY", "placeholder-for-parsing");
    let config =
        jk_config::Config::from_path(root.join("config.yaml")).expect("the shipped config parses");
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
         `enabled: true` — build the component, or comment it out."
    );
    assert!(
        config.instances.len() >= 10,
        "only {} instances parsed, so this check went quiet",
        config.instances.len()
    );
}

/// Doc pages covered by these checks: `docs/` and each package's `docs/`, excluding the changelog.
fn doc_pages(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = ["docs", "docs/concepts", "docs/guides"]
        .iter()
        .map(|d| root.join(d))
        .collect();
    for group in ["host", "clients", "extensions"] {
        let group = root.join("pkgs").join(group);
        dirs.push(group.join("docs"));
        if let Ok(entries) = std::fs::read_dir(&group) {
            dirs.extend(entries.flatten().map(|e| e.path().join("docs")));
        }
    }
    let mut pages = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
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
    pages
}

/// Every command the docs tell a user to run exists. The changelog is excluded because it records history.
#[test]
fn every_documented_command_exists() {
    let root = common::repo_root();
    let main_rs = std::fs::read_to_string(root.join("pkgs/host/host/src/main.rs"))
        .expect("the gateway's main is readable");
    let makefile = std::fs::read_to_string(root.join("Makefile")).expect("Makefile is readable");

    let mut pages = doc_pages(&root);
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
                    // `make` also appears in prose ("make sure"); only check plausible
                    // targets.
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

/// No shell block passes positional paths to `serve`/`verify`/`telegram`; that overrides the lookup beside the installed binary.
#[test]
fn no_shell_block_recommends_positional_serve_paths() {
    let root = common::repo_root();
    let mut pages = vec![root.join("README.md")];
    pages.extend(doc_pages(&root));

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
            // Every subcommand that takes `[config] [ext]` positionally has the same footgun.
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

/// The release publishes the registry where `config.yaml`'s `registry.url` says it is (#139).
#[test]
fn the_release_publishes_the_registry_where_config_yaml_says_it_is() {
    let root = common::repo_root();
    let config = std::fs::read_to_string(root.join("config.yaml")).expect("config.yaml");
    let workflow = std::fs::read_to_string(root.join(".github/workflows/release.yml"))
        .expect("release.yml is readable");

    // `registry.url` names the index; its parent is the site root.
    let url = config
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("url: "))
        .expect("config.yaml names a registry.url")
        .trim()
        .to_owned();
    let (site, index_file) = url
        .rsplit_once('/')
        .expect("registry.url has a path, so it has a parent");

    // What the workflow told the generator to write into every entry's `url`.
    let generated_base = workflow
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("REGISTRY_URL=\""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("release.yml sets REGISTRY_URL for `make registry-index`")
        .to_owned();

    assert_eq!(
        generated_base,
        format!("{site}/ext"),
        "release.yml builds component URLs under {generated_base}, but \
         config.yaml serves the index from {site}/ — so every entry in the \
         published index would name a path the site does not have"
    );

    // And the copies that put the files at those paths.
    assert!(
        workflow.contains(&format!("_site/{index_file}")),
        "registry.url names `{index_file}` at the site root, and release.yml \
         does not copy the index there — `ext search` would 404"
    );
    assert!(
        workflow.contains("_site/ext/"),
        "the index's entries point at <site>/ext/, and release.yml does not \
         assemble that directory — the index would name components that 404"
    );
}

/// Every tool pin in a workflow matches `versions.mk` (#131, #166). Only a different version fails; an omitted tool does not.
#[test]
fn every_tool_a_workflow_pins_matches_versions_mk() {
    let root = common::repo_root();
    let versions = std::fs::read_to_string(root.join("versions.mk")).expect("versions.mk");

    let pin = |name: &str| -> String {
        versions
            .lines()
            .find_map(|line| line.trim().strip_prefix(&format!("{name} := ")))
            .unwrap_or_else(|| panic!("versions.mk defines {name}"))
            .trim()
            .to_owned()
    };

    // `wkg` ships as a bare binary, so it is matched by `tag:` rather than `tool@`.
    let expected = [
        ("cargo-deny@", pin("CARGO_DENY_VERSION")),
        ("cargo-audit@", pin("CARGO_AUDIT_VERSION")),
        ("cargo-cyclonedx@", pin("CARGO_CYCLONEDX_VERSION")),
        ("cargo-nextest@", pin("CARGO_NEXTEST_VERSION")),
        ("wasm-tools@", pin("WASM_TOOLS_VERSION")),
        ("tag: v", pin("WKG_VERSION")),
    ];

    let mut checked = 0;
    for workflow in ["ci.yml", "ci-macos.yml", "release.yml", "labels.yml"] {
        let path = root.join(".github/workflows").join(workflow);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line_no, line) in text.lines().enumerate() {
            // Comments may name old versions as history; only installed versions count.
            if line.trim_start().starts_with('#') {
                continue;
            }
            for (marker, version) in &expected {
                let mut rest = line;
                while let Some(at) = rest.find(marker) {
                    rest = &rest[at + marker.len()..];
                    let found: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    assert_eq!(
                        &found,
                        version,
                        "{workflow}:{} installs `{marker}{found}` but versions.mk \
                         pins {version} — the two legs would resolve different \
                         tools, which is what #131 and #166 were each about",
                        line_no + 1
                    );
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked >= 6,
        "only {checked} pins were found in the workflows, so this test is \
         mostly not looking at anything — the spellings above have drifted"
    );
}
