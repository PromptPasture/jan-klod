//! `@` path completion.

use std::path::Path;

/// How many paths the list offers before it says there are more.
pub const MAX_ENTRIES: usize = 50;

/// What `@` is offering, and whether the list is all of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Paths relative to the working directory.
    pub entries: Vec<String>,
    /// Set when [`MAX_ENTRIES`] cut the list short.
    pub truncated: bool,
}

/// The run from the last `@` to the caret, or `None` if there is no `@` or whitespace intervenes.
#[must_use]
pub fn fragment(text: &str, caret: usize) -> Option<&str> {
    let head = text.get(..caret)?;
    let at = head.rfind('@')?;
    let candidate = &head[at + 1..];
    (!candidate.contains(char::is_whitespace)).then_some(candidate)
}

/// Paths under `root` matching `fragment`, honouring `.gitignore`.
#[must_use]
pub fn complete(root: &Path, fragment: &str) -> Completion {
    let mut entries = Vec::new();
    let mut truncated = false;

    for found in ignore::WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .build()
    {
        let Ok(entry) = found else { continue };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        let shown = relative.to_string_lossy().replace('\\', "/");
        if !shown.contains(fragment) {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(shown);
    }
    entries.sort();
    Completion { entries, truncated }
}

#[cfg(test)]
mod tests {
    use super::{complete, fragment, Completion, MAX_ENTRIES};

    /// The name must not come from the clock: trees collided at macOS's microsecond resolution.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tree() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jk-paths-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("src")).expect("tree");
        std::fs::create_dir_all(root.join("target")).expect("tree");
        std::fs::write(root.join(".gitignore"), "target/\nsecret.txt\n").expect("gitignore");
        std::fs::write(root.join("src/main.rs"), "").expect("file");
        std::fs::write(root.join("src/lib.rs"), "").expect("file");
        std::fs::write(root.join("README.md"), "").expect("file");
        std::fs::write(root.join("secret.txt"), "").expect("file");
        std::fs::write(root.join("target/huge.bin"), "").expect("file");
        root
    }

    #[test]
    fn the_fragment_is_the_run_from_the_last_at_to_the_caret() {
        let text = "look at @src/ma";
        assert_eq!(fragment(text, text.len()), Some("src/ma"));

        let mid = "see @src/lib.rs and also";
        assert_eq!(fragment(mid, 15), Some("src/lib.rs"));

        assert_eq!(fragment("see @src and", 12), None);
        assert_eq!(fragment("plain words", 11), None);
        assert_eq!(fragment("@", 1), Some(""));
    }

    #[test]
    fn gitignored_paths_are_not_offered() {
        let root = tree();
        let all = complete(&root, "");
        assert!(all.entries.iter().any(|e| e == "src/main.rs"));
        assert!(all.entries.iter().any(|e| e == "README.md"));
        assert!(
            !all.entries.iter().any(|e| e.starts_with("target")),
            "`target/` is ignored and was offered: {:?}",
            all.entries
        );
        assert!(
            !all.entries.iter().any(|e| e == "secret.txt"),
            "a file the user believes is hidden was offered"
        );
        assert!(
            !all.entries.iter().any(|e| e == ".gitignore"),
            "hidden files are skipped too"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dot_dot_cannot_be_walked_into() {
        let root = tree();
        for escape in ["../", "../../etc", "src/../../etc", ".."] {
            assert_eq!(
                complete(&root, escape),
                Completion::default(),
                "{escape:?} offered something outside the working directory"
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn matching_is_a_substring_because_a_path_hint_is_not_a_prefix() {
        let root = tree();
        let hits = complete(&root, "main");
        assert_eq!(
            hits.entries,
            vec!["src/main.rs".to_string()],
            "`main` must find `src/main.rs`, which a prefix never would"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_entry_cap_binds_and_says_so() {
        let root = tree();
        for i in 0..(MAX_ENTRIES + 20) {
            std::fs::write(root.join(format!("file{i:03}.txt")), "").expect("file");
        }
        let hits = complete(&root, "file");
        assert_eq!(hits.entries.len(), MAX_ENTRIES, "the cap bound");
        assert!(
            hits.truncated,
            "the list stopped and did not say so — #145 is exactly this defect \
             one layer down"
        );

        let few = complete(&root, "README");
        assert!(!few.truncated, "a complete list must not claim otherwise");
        std::fs::remove_dir_all(&root).ok();
    }
}
