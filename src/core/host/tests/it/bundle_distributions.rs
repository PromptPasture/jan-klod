//! Each named distribution boots with its own config (#114).
//!
//! A distribution is a guest list plus a `config.yaml` under
//! `scripts/distributions/`. `make bundle DIST=<name>` assembles them and
//! `scripts/bundle.sh` runs `jan-klod-gateway verify` inside the assembled
//! directory *before* tarring — so an archive that does not boot is never
//! produced.
//!
//! **This file is the repeatable half of that.** Running `make bundle` three
//! times by hand proves three archives booted once, on one machine, the day
//! somebody remembered. These tests ask the same question of every distribution
//! on every `make gate`, which is what Acceptance line 2 means by "asserted per
//! distribution, since three archives that are never booted is exactly the check
//! that reads green while proving nothing".
//!
//! What it does **not** cover is the tarball's shape — the layout an installed
//! bundle has on disk. `installed_layout.rs` covers that once, for the default
//! bundle, by assembling a real prefix and asking the running gateway for
//! `/health`.
//!
//! Distributions are discovered by reading the directory, not listed here, so a
//! fourth one is covered the day it is added.
//!
//! Offline: `verify` never calls a provider, and the API-key variables are set
//! to placeholders exactly as `bundle.sh` does.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::path::{Path, PathBuf};

use jan_klod_core::Runtime;

use crate::common;

/// Every distribution under `scripts/distributions/`, by name.
fn distributions() -> Vec<String> {
    let dir = common::repo_root().join("scripts/distributions");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("scripts/distributions is readable")
        .flatten()
        .filter(|e| e.path().join("config.yaml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no distributions found in {} — this test would otherwise pass by \
         checking nothing",
        dir.display()
    );
    names
}

/// Stage one distribution's guests the way `make bundle DIST=` does, by running
/// the same script rather than a reimplementation of it.
fn stage(name: &str, into: &Path) {
    let root = common::repo_root();
    let status = std::process::Command::new("sh")
        .arg(root.join("scripts/dist-stage.sh"))
        .arg(name)
        .arg(root.join("ext"))
        .arg(into)
        .status()
        .expect("dist-stage.sh runs");
    assert!(status.success(), "dist-stage.sh failed for {name}");
}

/// The placeholders `bundle.sh` sets before verifying. `verify` resolves config
/// but never calls a provider, so any non-empty value does.
fn set_placeholder_keys() {
    for var in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
        if std::env::var(var).is_err() {
            std::env::set_var(var, "placeholder");
        }
    }
}

fn scratch(tag: &str) -> (PathBuf, common::TempDir) {
    let dir = std::env::temp_dir().join(format!("jk-dist-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the scratch dir");
    (dir.clone(), common::TempDir(dir))
}

/// Acceptance line 2: every distribution boots offline with its own config.
///
/// One test over all of them rather than three tests: the set is read from
/// disk, so a per-distribution test would either be generated or would go stale
/// the moment a fourth is added. The assertion names which one failed, which is
/// what a separate test would have bought.
#[test]
fn every_distribution_boots_with_its_own_config() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-system.wasm"]) {
        return;
    }
    set_placeholder_keys();
    let root = common::repo_root();

    for name in distributions() {
        let (dir, _guard) = scratch(&name);
        let ext = dir.join("ext");
        stage(&name, &ext);

        let config = root
            .join("scripts/distributions")
            .join(&name)
            .join("config.yaml");
        Runtime::boot(&config, &ext).unwrap_or_else(|err| {
            panic!("distribution `{name}` does not boot with its own config: {err}")
        });

        // **Booting is not enough, and finding that out is why this is here.**
        // An enabled instance whose component is absent does not fail the boot —
        // it is simply never loaded. So a distribution could enable `tool.fs`,
        // omit `tool-fs` from its guest list, boot green, and hand a user an
        // install where the tool they came for silently does not exist. The
        // guest list and the config have to agree, and only this says so.
        let parsed = jan_klod_config::Config::from_path(&config)
            .unwrap_or_else(|err| panic!("distribution `{name}`'s config parses: {err}"));
        let missing: Vec<String> = parsed
            .instances
            .iter()
            .filter(|instance| !ext.join(instance.component_file()).exists())
            .map(|instance| format!("{} -> {}", instance.id, instance.component_file()))
            .collect();
        assert!(
            missing.is_empty(),
            "distribution `{name}` enables instances it does not ship: {missing:?} — \
             add them to scripts/distributions/{name}/guests, or stop enabling them"
        );
        assert!(
            !parsed.instances.is_empty(),
            "distribution `{name}` enables nothing, so the check above went quiet"
        );
    }
}

/// `headless-chat` runs no commands, and this is the guard that says so.
///
/// Decision (b) on #114: the config states `execution: enabled: false` once, in
/// the form that means it, rather than stacking `require: true` with a
/// contradictory sandbox mode to deny twice. A second lock whose label lies
/// about *why* would invite the next reader to delete it while "fixing" a
/// sandbox this distribution never asked for. This test is the belt-and-braces
/// instead — it fails loudly where a second lock would hold quietly.
#[test]
fn headless_chat_does_not_run_commands() {
    let config = common::repo_root().join("scripts/distributions/headless-chat/config.yaml");
    let text = std::fs::read_to_string(&config).expect("the config is readable");
    // The `execution:` block specifically — `enabled:` appears under every
    // provider and interceptor too, so searching the whole file for
    // `enabled: false` would pass on somebody else's line.
    let block = text.split_once("\nexecution:").map_or_else(
        || {
            panic!(
                "headless-chat must state an `execution:` block: {}",
                config.display()
            )
        },
        |(_, rest)| rest,
    );
    let setting = block
        .lines()
        .find_map(|l| l.trim().strip_prefix("enabled:"))
        .map_or_else(
            || panic!("the `execution:` block must say whether it is enabled"),
            str::trim,
        );
    assert_eq!(
        setting, "false",
        "headless-chat runs no commands, so its execution block must say so"
    );
    // The tools that would need it are absent too, so a careless `enabled: true`
    // still reaches nothing — but the line above is the one that says the intent.
    let guests = std::fs::read_to_string(
        common::repo_root().join("scripts/distributions/headless-chat/guests"),
    )
    .expect("the guest list is readable");
    for tool in ["tool-shell", "tool-fs", "tool-edit", "tool-git"] {
        assert!(
            !guests.lines().any(|l| l.trim() == tool),
            "headless-chat ships {tool}, which is a thing that touches the machine"
        );
    }
}
