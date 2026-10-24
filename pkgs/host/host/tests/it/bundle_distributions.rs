//! Every distribution under `scripts/distributions/` boots offline and its guest list agrees with its config.

use std::path::{Path, PathBuf};

use jk_agent::Runtime;

use crate::common;

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

        let parsed = jk_config::Config::from_path(&config)
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

#[test]
fn headless_chat_does_not_run_commands() {
    let config = common::repo_root().join("scripts/distributions/headless-chat/config.yaml");
    let text = std::fs::read_to_string(&config).expect("the config is readable");
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
