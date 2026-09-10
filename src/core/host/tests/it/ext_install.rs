//! `ext install` against real components.
//!
//! The refusals that need only bytes — not a component, no manifest, already
//! installed — are unit tests in `core::ext`, where they are cheaper. What
//! cannot be tested there is anything requiring a *real* component: that a
//! valid one lands and is loadable afterwards, and that a manifest which
//! disagrees with the component's actual imports is refused. Both need imports
//! to disagree *about*, and fabricated bytes have none.

use std::path::{Path, PathBuf};

use jan_klod_core::ext::{self, Declaration, ExtError};

use crate::common;

/// A scratch `ext/` and a place to put sources, removed on drop.
struct Scratch {
    root: PathBuf,
    ext: PathBuf,
    incoming: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch(tag: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("jk-ext-install-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ext = root.join("ext");
    let incoming = root.join("incoming");
    std::fs::create_dir_all(&ext).expect("creates a scratch ext/");
    std::fs::create_dir_all(&incoming).expect("creates an incoming dir");
    Scratch {
        root,
        ext,
        incoming,
    }
}

/// Copy a staged guest and its manifest into `incoming`, returning the
/// component's path — an installable pair, exactly as a release would ship it.
fn offer(scratch: &Scratch, guest: &str) -> PathBuf {
    let staged = common::repo_root().join("ext");
    let component = scratch.incoming.join(format!("{guest}.wasm"));
    std::fs::copy(staged.join(format!("{guest}.wasm")), &component).expect("copies the component");
    std::fs::copy(
        staged.join(format!("{guest}.manifest.toml")),
        scratch.incoming.join(format!("{guest}.manifest.toml")),
    )
    .expect("copies the manifest");
    component
}

/// Every name in a directory, sorted — enough to assert "nothing landed".
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("reads the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_valid_component_lands_with_its_manifest_and_is_then_loadable() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("valid");
    let source = offer(&scratch, "tool-fs");

    let installed = ext::install(&scratch.ext, &source).expect("a real component installs");
    assert_eq!(installed.name, "tool-fs");

    // Both files, and nothing else — no staging directory left over.
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair lands together and the staging directory is gone"
    );

    // The declaration it reports is the one now on disk.
    let Declaration::Present(manifest) = &installed.declaration else {
        panic!("a component that installed has a readable manifest: {installed:?}")
    };
    assert_eq!(manifest.capabilities, ["host-fs"]);

    // And `list` — a separate reader — agrees about what landed.
    let listed = ext::list(&scratch.ext).expect("lists");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].declaration, installed.declaration);

    // The claim worth more than either: what installed is *loadable*. Compiling
    // it is what the boot path does, so this is the check that an install
    // cannot succeed on something the runtime would then reject.
    let engine = wasmtime::Engine::default();
    wasmtime::component::Component::from_file(&engine, scratch.ext.join("tool-fs.wasm"))
        .expect("what install accepted, the runtime can load");
}

/// The refusal that needs a real component: a manifest which does not admit to
/// what the component imports. This is the check that makes an install worth
/// more than a checksum — a component's declaration is what an operator reads
/// before granting anything, so one that under-declares must not land.
#[test]
fn a_manifest_that_under_declares_is_refused_and_nothing_lands() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("under-declared");
    let source = offer(&scratch, "tool-fs");

    // tool-fs really imports `host-fs`; say it needs nothing.
    let manifest = scratch.incoming.join("tool-fs.manifest.toml");
    let text = std::fs::read_to_string(&manifest).expect("reads the manifest");
    assert!(
        text.contains("\"host-fs\""),
        "the fixture removes this line, so it has to be there: {text}"
    );
    std::fs::write(
        &manifest,
        text.replace("\"host-fs\",", "").replace("\"host-fs\"", ""),
    )
    .expect("rewrites the manifest");

    let err = ext::install(&scratch.ext, &source).expect_err("must refuse");
    let ExtError::UnderDeclared { interfaces, .. } = &err else {
        panic!("an under-declaring manifest is refused as such: {err:?}")
    };
    assert!(
        interfaces.contains("host-fs"),
        "the refusal names the interface that was hidden: {interfaces}"
    );
    assert_eq!(
        names(&scratch.ext),
        Vec::<String>::new(),
        "nothing landed, and no staging directory survived"
    );
}

/// A manifest built against another interface package is refused too, and
/// distinguishably — the same gate boot applies, reached through `install`.
#[test]
fn a_manifest_from_another_api_version_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("api");
    let source = offer(&scratch, "tool-fs");

    let manifest = scratch.incoming.join("tool-fs.manifest.toml");
    let text = std::fs::read_to_string(&manifest).expect("reads the manifest");
    std::fs::write(
        &manifest,
        text.replace(
            &format!("api-version = \"{}\"", jan_klod_core::manifest::API_VERSION),
            "api-version = \"9.0.0\"",
        ),
    )
    .expect("rewrites the manifest");

    let err = ext::install(&scratch.ext, &source).expect_err("must refuse");
    let ExtError::ApiMismatch { theirs, ours, .. } = &err else {
        panic!("an incompatible package version is refused as such: {err:?}")
    };
    assert_eq!(theirs, "9.0.0");
    assert_eq!(ours, jan_klod_core::manifest::API_VERSION);
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}
