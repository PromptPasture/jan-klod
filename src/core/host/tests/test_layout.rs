//! Every test directory in this repo belongs to a crate that cargo builds.
//!
//! `src/core/tests/` held eighteen tests across three files and none of them ran.
//! The directory sits beside `src/core/Cargo.toml`, which is a **virtual**
//! manifest — `[workspace]` with no `[package]` — so it belongs to no crate, and
//! cargo silently builds nothing from it. `cargo test` was green throughout,
//! because a test that is never compiled cannot fail. They were orphaned by the
//! reorganisation that split the repo into `src/core` + `src/extensions`; nothing
//! in a review diff makes a moved directory look unreachable.
//!
//! This is the third variant of the same failure — the pre-commit hook deleting
//! `ext/` so guest tests skipped, `JK_REQUIRE_GUESTS` for tests that skip
//! themselves, and now tests that are not built at all. Each was invisible for the
//! same reason: the suite's own report is the thing under test, and it says
//! "passed" either way.

use std::path::{Path, PathBuf};

mod common;

/// Directories that are not ours to police.
const IGNORED: [&str; 4] = ["target", ".git", "node_modules", "ext"];

/// Every `tests/` directory under `root`, with the nearest manifest above it.
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

/// Whether `dir/Cargo.toml` declares a package (as opposed to a bare workspace).
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
        match is_package(parent) {
            // A `tests/` beside a package manifest: cargo builds it. Correct.
            Some(true) => {}
            // A `tests/` beside a virtual workspace manifest: cargo builds nothing.
            Some(false) => orphans.push(dir.clone()),
            // No manifest at all — e.g. `wit/tests` or a fixture directory. Cargo
            // was never going to build it and nobody expects it to.
            None => {}
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
