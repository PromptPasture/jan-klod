//! `@` path completion (#153).
//!
//! # It offers a path and reads nothing
//!
//! The accepted text is inserted into the message as a plain path. The client
//! does **not** open the file. Attachments, file contents and any notion of
//! "context" are core-side concerns — a client that starts reading files has
//! grown policy, and policy lives in `interceptor-*` guests in this design, not
//! in the things around them.
//!
//! # The fragment can be anywhere in the message
//!
//! This is the first difference from the `/` menu. A command is the whole
//! buffer, so #152 derives "is the menu open" from the buffer's shape. A path
//! reference is mid-sentence — `look at @src/main.rs and tell me` — so the
//! fragment is the run from the last `@` back to the caret, and it ends at the
//! first whitespace because a path with a space in it is not what `@` is for.
//!
//! # Two bounds, and one of them has to be visible
//!
//! The walk honours `.gitignore` and stops at [`MAX_ENTRIES`]. That cap is the
//! interesting one: [#145](https://github.com/PromptPasture/jan-klod/issues/145)
//! is open right now because a bounded walk elsewhere in this repository answers
//! from the first 500 files and the caller cannot tell. [`Completion::truncated`]
//! exists so this list cannot make the same mistake quietly.
//!
//! # Why `..` cannot be walked into
//!
//! Structurally, rather than by a check that could be forgotten: the walk only
//! ever descends **from** the working directory, and the fragment is matched
//! against the relative paths it yields. `@../secret` matches nothing because
//! nothing under the root is spelled that way. A check on the typed string would
//! have to catch `a/../../..` and every other spelling; this has nothing to
//! catch.
//!
//! Worth being exact about what that is: a client-side path rule is
//! **convenience, not security**. The security model's own line is that a check
//! inside the sandbox is advice rather than a boundary, and the same reasoning
//! applies here. It is still the difference between offering somebody their own
//! files and offering them the filesystem.

use std::path::Path;

/// How many paths the list offers before it says there are more.
pub const MAX_ENTRIES: usize = 50;

/// What `@` is offering, and whether the list is all of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Paths relative to the working directory.
    pub entries: Vec<String>,
    /// Set when [`MAX_ENTRIES`] cut the list short. A list that stops silently
    /// is a list a caller believes is complete.
    pub truncated: bool,
}

/// The fragment being completed: the run from the last `@` back to the caret.
///
/// `None` when the caret is not in one — no `@` before it, or whitespace
/// between the two, which means the reference has ended.
#[must_use]
pub fn fragment(text: &str, caret: usize) -> Option<&str> {
    let head = text.get(..caret)?;
    let at = head.rfind('@')?;
    let candidate = &head[at + 1..];
    (!candidate.contains(char::is_whitespace)).then_some(candidate)
}

/// Paths under `root` matching `fragment`, honouring `.gitignore`.
///
/// A **substring** match, unlike the `/` menu's prefix: `@main` should find
/// `src/main.rs`, which a prefix never would, and a path fragment is a hint
/// about a file rather than the start of its name.
#[must_use]
pub fn complete(root: &Path, fragment: &str) -> Completion {
    let mut entries = Vec::new();
    let mut truncated = false;

    // `ignore`'s walk is lazy, so the cap stops the traversal rather than
    // trimming its result — on a large repository that is the difference
    // between a completion list and a pause.
    // `require_git(false)` is load-bearing and was found by a failing test.
    // `WalkBuilder` honours `.gitignore` **only inside a git repository** by
    // default — so in any working directory that is not one, `target/` and
    // everything else would have been offered while the code read as though it
    // filtered. The issue asks to skip what `.gitignore` names "where one is
    // present", which is the repo-independent reading, and it is also the safer
    // default: a user who wrote a `.gitignore` meant it.
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

    /// Distinguishes two trees built in the same microsecond.
    ///
    /// The name used to be process id + `SystemTime::now().as_nanos()`, which
    /// reads as unique and is not: macOS's clock has **microsecond**
    /// granularity, so `as_nanos()` there always ends in three zeros and 179 of
    /// 200 consecutive readings are identical. Rust runs these tests as threads
    /// of one process, so two that called `tree()` together got the same pid
    /// and the same reading, and therefore *the same directory* — one test's
    /// 70 `fileNNN.txt` fixtures landing in another's tree, and whichever
    /// finished first deleting the other's root mid-walk with `remove_dir_all`.
    ///
    /// That made `the_entry_cap_binds_and_says_so` fail about one run in six,
    /// and only when run alongside its siblings — never alone, which is the
    /// signature of shared state rather than of a wrong assertion.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn tree() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "jk-paths-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // The counter restarts every process, so a run whose test panicked
        // before its `remove_dir_all` leaves a tree that a later run with a
        // recycled pid would inherit. Start from nothing rather than from
        // whatever survived.
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

        // Mid-message, with more after the caret.
        let mid = "see @src/lib.rs and also";
        assert_eq!(fragment(mid, 15), Some("src/lib.rs"));

        // A space ends the reference.
        assert_eq!(fragment("see @src and", 12), None);
        // No `@` at all.
        assert_eq!(fragment("plain words", 11), None);
        // The `@` itself offers everything.
        assert_eq!(fragment("@", 1), Some(""));
    }

    /// Acceptance line 1: an ignored path is not offered.
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

    /// Acceptance line 2, and it holds structurally rather than by a check.
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

    /// Acceptance line 3: the cap binds, and the user can tell.
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
