//! Reading and unmaking what is staged in `ext/`.
//!
//! `list` and `remove` only. Installing is a later slice
//! ([#91](https://github.com/PromptPasture/jan-klod/issues/91)'s remaining
//! boxes) and deliberately absent here: an `install` that copies before it can
//! verify is the thing this whole slice exists to replace, and a half-written
//! one is worse than none because `ext/` would already contain its output.
//!
//! # Why `list` reads manifests rather than filenames
//!
//! `ls ext/` already lists files. What an operator cannot see that way is what
//! each component may *ask the host for* — and that is the number that matters,
//! since a capability is granted by being declared and cross-checked at boot
//! rather than by anything visible in the filename.

use std::path::{Path, PathBuf};

use crate::manifest::{manifest_path, Manifest};

/// The extension of a staged component.
const COMPONENT_EXT: &str = "wasm";

/// What a component's manifest turned out to be.
///
/// Three states rather than an `Option`, because "absent" and "present but
/// unusable" are different problems with different fixes, and collapsing them
/// would report a typo in a manifest as a missing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// A manifest that reads.
    Present(Manifest),
    /// No manifest file beside the component. The boot path refuses this.
    Absent,
    /// A manifest is there and could not be used; the reason, rendered.
    Broken(String),
}

/// One staged component, and what it declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// File stem — the name [`remove`] takes.
    pub name: String,
    /// The component file itself.
    pub component: PathBuf,
    /// Its manifest, or why there is not a usable one.
    pub declaration: Declaration,
}

/// Why an `ext` operation could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    /// The extension directory could not be read.
    #[error("reading {path}")]
    Unreadable {
        /// The directory that could not be read.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Nothing by that name is staged.
    #[error("no component named {name} in {dir}")]
    NotInstalled {
        /// The name that was asked for.
        name: String,
        /// Where it was looked for.
        dir: String,
    },
    /// A file existed and could not be deleted.
    #[error("removing {path}")]
    Undeletable {
        /// The file that could not be removed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Every staged component in `dir`, sorted by name.
///
/// A single unusable manifest does not fail the whole listing — it is reported
/// against its own component as [`Declaration::Broken`]. Refusing to list
/// anything because one file has a typo would break the command precisely when
/// it is most wanted, since a broken manifest is a reason to run `list`.
///
/// # Errors
/// [`ExtError::Unreadable`] if `dir` cannot be read at all. A directory that
/// does not exist is *not* an error: nothing staged is a legitimate state, and
/// it lists as empty.
pub fn list(dir: &Path) -> Result<Vec<Installed>, ExtError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir).map_err(|source| ExtError::Unreadable {
        path: dir.display().to_string(),
        source,
    })?;

    let mut staged = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ExtError::Unreadable {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(COMPONENT_EXT) {
            continue;
        }
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let declaration = match Manifest::beside(&path) {
            Ok(Some(manifest)) => Declaration::Present(manifest),
            Ok(None) => Declaration::Absent,
            Err(err) => Declaration::Broken(err.to_string()),
        };
        staged.push(Installed {
            name,
            component: path,
            declaration,
        });
    }
    staged.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(staged)
}

/// What [`remove`] deleted.
///
/// An enum rather than two booleans so that "neither" cannot be expressed:
/// [`remove`] refuses that case with [`ExtError::NotInstalled`], and a struct
/// of two flags would leave every caller with a fourth branch to handle that
/// can never happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    /// The component and its manifest, as a healthy install has both.
    Both,
    /// A component that had no manifest — the boot path would have refused it.
    ComponentOnly,
    /// A manifest with no component beside it: an orphan declaration.
    ManifestOnly,
}

/// Delete a staged component **and** its manifest.
///
/// Both, always. Removing only the component leaves a manifest that the next
/// `list` believes and that describes nothing — an orphan declaration is a
/// worse state than either file alone, because it reads as a component that is
/// merely misplaced.
///
/// Either file may already be missing, and the pair that was actually deleted
/// is reported rather than assumed: that is how a caller can say "removed an
/// orphan manifest" instead of implying a component was there.
///
/// # Errors
/// [`ExtError::NotInstalled`] when neither file exists — nothing was asked for
/// that could be removed. [`ExtError::Undeletable`] if a file that is there
/// cannot be deleted.
pub fn remove(dir: &Path, name: &str) -> Result<Removed, ExtError> {
    let component = dir.join(format!("{name}.{COMPONENT_EXT}"));
    let manifest = manifest_path(&component);

    let had_component = component.exists();
    let had_manifest = manifest.exists();
    if !had_component && !had_manifest {
        return Err(ExtError::NotInstalled {
            name: name.to_owned(),
            dir: dir.display().to_string(),
        });
    }

    // The component first: while both exist, a failure part-way leaves the
    // manifest describing a component that is gone, which `list` reports as
    // broken staging rather than silently.
    if had_component {
        std::fs::remove_file(&component).map_err(|source| ExtError::Undeletable {
            path: component.display().to_string(),
            source,
        })?;
    }
    if had_manifest {
        std::fs::remove_file(&manifest).map_err(|source| ExtError::Undeletable {
            path: manifest.display().to_string(),
            source,
        })?;
    }
    Ok(match (had_component, had_manifest) {
        (true, true) => Removed::Both,
        (true, false) => Removed::ComponentOnly,
        // The `!had_component && !had_manifest` case returned above, so this
        // arm is the manifest-only one and the match stays wildcard-free.
        (false, _) => Removed::ManifestOnly,
    })
}

#[cfg(test)]
mod tests {
    use super::{list, remove, Declaration, ExtError, Removed};
    use std::path::{Path, PathBuf};

    /// Removes the directory on drop, panic or not.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("jk-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates a temp dir");
        TempDir(dir)
    }

    /// A component file and, optionally, the manifest beside it. The bytes are
    /// never parsed by `list`, which reads declarations rather than components.
    fn stage(dir: &Path, name: &str, manifest: Option<&str>) {
        std::fs::write(dir.join(format!("{name}.wasm")), b"\0asm").expect("writes a component");
        if let Some(body) = manifest {
            std::fs::write(dir.join(format!("{name}.manifest.toml")), body)
                .expect("writes a manifest");
        }
    }

    fn manifest_for(name: &str, capabilities: &str) -> String {
        format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\napi-version = \"0.1.0\"\n\
             kind = \"tool\"\ndescription = \"\"\ncapabilities = [{capabilities}]\n"
        )
    }

    #[test]
    fn an_absent_directory_lists_as_empty_rather_than_failing() {
        let dir = temp_dir("absent");
        let missing = dir.0.join("never-created");
        assert_eq!(list(&missing).expect("nothing staged is not an error"), []);
    }

    #[test]
    fn components_are_listed_by_name_with_what_they_declare() {
        let dir = temp_dir("list");
        stage(&dir.0, "tool-zed", Some(&manifest_for("tool-zed", "")));
        stage(
            &dir.0,
            "tool-abc",
            Some(&manifest_for("tool-abc", "\"host-fs\"")),
        );
        // Not a component, so not staged — `list` must not report it.
        std::fs::write(dir.0.join("notes.txt"), "ignored").expect("writes a stray file");

        let staged = list(&dir.0).expect("lists");
        let names: Vec<&str> = staged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["tool-abc", "tool-zed"],
            "sorted, and only the .wasm"
        );

        let Declaration::Present(manifest) = &staged[0].declaration else {
            panic!("tool-abc has a readable manifest: {:?}", staged[0])
        };
        assert_eq!(manifest.capabilities, ["host-fs"]);

        let Declaration::Present(empty) = &staged[1].declaration else {
            panic!("tool-zed has a readable manifest")
        };
        assert!(
            empty.capabilities.is_empty(),
            "an empty list is a claim, not an absence"
        );
    }

    /// The two unhappy manifests are distinguished, because the fixes differ:
    /// one needs generating, the other needs correcting.
    #[test]
    fn a_missing_manifest_and_an_unusable_one_are_different_states() {
        let dir = temp_dir("declarations");
        stage(&dir.0, "tool-bare", None);
        stage(&dir.0, "tool-broken", Some("this is not toml{{{"));

        let staged = list(&dir.0).expect("one bad manifest does not fail the listing");
        assert_eq!(staged.len(), 2, "both are still reported: {staged:?}");
        assert_eq!(staged[0].declaration, Declaration::Absent);
        let Declaration::Broken(reason) = &staged[1].declaration else {
            panic!("a present-but-unparseable manifest is Broken: {staged:?}")
        };
        assert!(
            reason.contains("TOML"),
            "the reason names what is wrong: {reason}"
        );
    }

    #[test]
    fn removing_takes_the_component_and_the_manifest() {
        let dir = temp_dir("remove");
        stage(&dir.0, "tool-fs", Some(&manifest_for("tool-fs", "")));
        assert_eq!(remove(&dir.0, "tool-fs").expect("removes"), Removed::Both);
        assert!(!dir.0.join("tool-fs.wasm").exists());
        assert!(
            !dir.0.join("tool-fs.manifest.toml").exists(),
            "a manifest left behind is an orphan declaration the next list believes"
        );
        assert_eq!(list(&dir.0).expect("lists"), []);
    }

    /// An orphan manifest is removable on its own, and says so — otherwise the
    /// only way to clear one would be by hand.
    #[test]
    fn each_file_can_be_removed_without_the_other() {
        let dir = temp_dir("partial");
        stage(&dir.0, "tool-bare", None);
        assert_eq!(
            remove(&dir.0, "tool-bare").expect("removes"),
            Removed::ComponentOnly
        );

        std::fs::write(
            dir.0.join("tool-ghost.manifest.toml"),
            manifest_for("tool-ghost", ""),
        )
        .expect("writes an orphan manifest");
        assert_eq!(
            remove(&dir.0, "tool-ghost").expect("removes"),
            Removed::ManifestOnly
        );
    }

    #[test]
    fn removing_something_that_is_not_there_is_refused_by_name() {
        let dir = temp_dir("absent-remove");
        let Err(ExtError::NotInstalled { name, .. }) = remove(&dir.0, "tool-nope") else {
            panic!("nothing to remove must be an error, not a silent success")
        };
        assert_eq!(name, "tool-nope");
    }
}
