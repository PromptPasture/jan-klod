//! Tests must belong to buildable crates. `tests/` beside a **virtual**
//! manifest gets silently skipped by cargo (no compilation = green test).
//! Walks all `tests/` dirs and flags those beside virtual manifests.

use std::path::{Path, PathBuf};

use crate::common;

/// Directories that are not ours to police.
const IGNORED: [&str; 4] = ["target", ".git", "node_modules", "ext"];

/// Collect all `tests/` dirs under root.
fn test_dirs(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if IGNORED.contains(&name.as_ref()) || name.starts_with('.') {
            continue;
        }
        if name == "tests" {
            found.push(path.clone());
        }
        test_dirs(&path, found);
    }
}

/// Does dir have package manifest (not bare workspace)?
fn is_package(dir: &Path) -> Option<bool> {
    let manifest = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(manifest).ok()?;
    Some(text.lines().any(|line| line.trim() == "[package]"))
}

#[test]
fn every_tests_directory_belongs_to_a_crate_cargo_builds() {
    let root = common::repo_root();
    let mut dirs = Vec::new();
    test_dirs(&root, &mut dirs);
    assert!(
        !dirs.is_empty(),
        "the walk found no test directories at all — it is broken"
    );

    let mut orphans = Vec::new();
    for dir in &dirs {
        let Some(parent) = dir.parent() else { continue };
        // Beside package manifest (cargo builds) or no manifest (fixture).
        if is_package(parent) == Some(false) {
            // tests/ beside virtual manifest: cargo builds nothing.
            orphans.push(dir.clone());
        }
    }

    assert!(
        orphans.is_empty(),
        "these test directories sit beside a virtual workspace manifest, so cargo \
         compiles nothing in them and every test inside reports as passing: {}. \
         Move them into a member crate's own `tests/`.",
        orphans
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap_or(p).display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
