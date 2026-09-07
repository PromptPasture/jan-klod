//! Shared pure-Rust helpers for the `host-fs`-routed guests (`tool-fs`,
//! `tool-find`, `tool-edit`).
//!
//! This is a **library, not a component**: it holds no WIT bindings and imports
//! no capability. Guests stay separate sandboxed components; they merely compile
//! the same tested logic in rather than each keeping a copy — `truncate` had been
//! duplicated byte-for-byte in two guests, and the moment `tool-fs` learned to
//! grep a directory it would have needed `tool-find`'s walk as a third copy.
//!
//! What lives here is everything that is *policy about cost and shape* rather
//! than about a particular tool: how much output a single call may return, how a
//! glob matches, and how far a walk may go before it stops and says so.

/// Result byte cap — one tool call must not consume the whole context budget.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Most paths reported from a single walk.
pub const MAX_RESULTS: usize = 500;
/// Most directory entries examined during one walk.
pub const MAX_VISITS: usize = 20_000;
/// Deepest directory nesting descended into (relative to the search root).
pub const MAX_DEPTH: usize = 24;

/// Directories skipped unless the pattern names them literally.
///
/// These are the build/vendor trees that dominate a walk's cost while almost never
/// being what the caller meant; naming one in the pattern (`target/**/*.rs`) opts
/// back in.
pub const PRUNED_DIRS: [&str; 6] = [
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".jj",
];

/// One directory entry, as `host-fs.entry` hands it over.
pub struct Entry {
    /// Bare file or directory name (not a path).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// What a walk found, and which bound (if any) cut it short.
pub struct Outcome {
    /// Matching paths, workspace-relative and sorted.
    pub paths: Vec<String>,
    /// Set when a bound stopped the walk before the tree was exhausted, so a
    /// caller never reads a partial answer as an exhaustive one.
    pub bounded_by: Option<&'static str>,
}

/// Cap `output` at [`MAX_OUTPUT_BYTES`], backing off to a valid UTF-8 boundary
/// and appending a truncation marker when it overflows.
#[must_use]
pub fn truncate(output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    // Back off to a valid UTF-8 boundary at or below the cap; slicing at a
    // non-boundary would panic.
    let mut safe_len = MAX_OUTPUT_BYTES;
    while safe_len > 0 && !output.is_char_boundary(safe_len) {
        safe_len -= 1;
    }
    format!(
        "{}\n…[truncated: {} bytes omitted]",
        &output[..safe_len],
        output.len() - safe_len
    )
}

/// Expand a slash-less pattern to search recursively.
///
/// Strict glob semantics make `*.rs` mean "top level only", which is almost
/// never what a caller (or a small model) means when it asks to *find* a file.
/// A pattern that spells out any structure is left exactly as written.
#[must_use]
pub fn normalize(pattern: &str) -> String {
    if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    }
}

/// Whether `path` matches glob `pattern`.
///
/// `*` and `?` match within one path segment; `**` matches any run of segments,
/// including none.
#[must_use]
pub fn matches(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &seg)
}

fn match_segments(pat: &[&str], seg: &[&str]) -> bool {
    match pat.split_first() {
        None => seg.is_empty(),
        Some((&"**", rest)) => (0..=seg.len()).any(|i| match_segments(rest, &seg[i..])),
        Some((head, rest)) => match seg.split_first() {
            Some((name, tail)) => match_name(head, name) && match_segments(rest, tail),
            None => false,
        },
    }
}

/// Match one path segment against a `*`/`?` pattern (neither crosses `/`).
fn match_name(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Backtrack point: the last `*` seen, and how much of `text` it had eaten.
    let mut star: Option<usize> = None;
    let mut eaten = 0;
    while ti < text.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            eaten = ti;
            pi += 1;
        } else if let Some(s) = star {
            // Let the `*` swallow one more char and retry from just after it.
            eaten += 1;
            ti = eaten;
            pi = s + 1;
        } else {
            return false;
        }
    }
    pat[pi..].iter().all(|c| *c == '*')
}

/// Whether the walk should descend into a directory named `name`.
///
/// Heavy build/vendor trees are pruned unless the pattern mentions them, so
/// opting back in is a matter of asking for them by name.
fn should_descend(name: &str, pattern: &str) -> bool {
    !PRUNED_DIRS.contains(&name) || pattern.split('/').any(|segment| segment == name)
}

/// Whether this file should be withheld from a walk's results.
///
/// Credential files are withheld — unless the pattern *names* one, the same rule
/// [`should_descend`] applies to pruned directories: asking for something by name
/// opts back into it. A pattern of `**/*` or `src/**/*.rs` does not name `.env`,
/// so a broad glob and every tree-wide grep still skip it. `**/.env` does.
///
/// The distinction matters because hiding a file unconditionally would teach the
/// model that it does not exist, and an agent that has been told there is no
/// `.env` will confidently tell the user the same. Listing a *name* discloses
/// nothing; the contents are what needed protecting, and reading them is gated.
fn hidden_credential(name: &str, pattern: &str) -> bool {
    is_credential_file(name) && !pattern.split('/').any(|segment| segment == name)
}

/// Whether a file's name marks it as holding credentials.
///
/// Reads and greps are on the permission gate's read-only allowlist, so they run
/// **without asking** — which is right for source code and wrong for `.env`. A
/// grep for `password` across a repository would otherwise return the contents of
/// the credential file that happens to be in it, and everything a tool returns
/// becomes a message in the transcript, which is sent to the model provider on
/// the next turn. The workspace's secrets would leave the machine because
/// somebody searched for a word.
///
/// So the shared walk skips these, which covers `find` and `grep` together — the
/// gate cannot help there, because it sees the *pattern*, not the files a pattern
/// will match. An explicit read by path still works and is still gated, so
/// nothing becomes impossible; it just stops being silent.
///
/// Matched on the file name only. This is a heuristic and named as one: it will
/// miss `config/production.yaml` holding a database URL. It covers the
/// conventional names, which is where the accident lives.
#[must_use]
pub fn is_credential_file(name: &str) -> bool {
    /// Exact names.
    const NAMES: [&str; 8] = [
        ".env",
        ".envrc",
        ".netrc",
        ".npmrc",
        ".pgpass",
        ".git-credentials",
        "credentials",
        "id_rsa",
    ];
    /// Suffixes, including the `.env.production` family.
    const SUFFIXES: [&str; 7] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".ppk"];

    let lower = name.to_lowercase();
    NAMES.contains(&lower.as_str())
        || SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
        || lower.starts_with(".env.")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
}

/// Walk `root` through `list`, collecting files matching `pattern`.
///
/// `list` is the `host-fs` `list-dir` seam (returning `None` when a directory
/// cannot be read — an unreadable subtree is skipped, never fatal). Paths are
/// matched *relative to `root`* and reported joined back onto it.
///
/// Every dimension is bounded ([`MAX_VISITS`], [`MAX_DEPTH`], [`MAX_RESULTS`]),
/// because a glob is the one file operation whose cost is set by the *tree*, not
/// by the argument: `**/*` in a monorepo must not be able to hang the turn.
pub fn walk(root: &str, pattern: &str, list: &dyn Fn(&str) -> Option<Vec<Entry>>) -> Outcome {
    let pattern = normalize(pattern);
    let mut paths = Vec::new();
    let mut bounded_by = None;
    let mut visits = 0usize;
    // (path relative to root, depth); "" is the root itself.
    let mut queue = vec![(String::new(), 0usize)];

    while let Some((dir, depth)) = queue.pop() {
        let Some(entries) = list(&join(root, &dir)) else {
            continue;
        };
        for entry in entries {
            visits += 1;
            if visits > MAX_VISITS {
                bounded_by = Some("visit budget");
                queue.clear();
                break;
            }
            let rel = join(&dir, &entry.name);
            if entry.is_dir {
                if depth + 1 < MAX_DEPTH && should_descend(&entry.name, &pattern) {
                    queue.push((rel, depth + 1));
                }
            } else if matches(&pattern, &rel) && !hidden_credential(&entry.name, &pattern) {
                if paths.len() == MAX_RESULTS {
                    bounded_by = Some("result cap");
                    queue.clear();
                    break;
                }
                paths.push(join(root, &rel));
            }
        }
    }

    paths.sort();
    Outcome { paths, bounded_by }
}

/// Join two workspace-relative fragments, treating `""` and `"."` as "here".
#[must_use]
pub fn join(base: &str, rest: &str) -> String {
    let base = if base == "." { "" } else { base };
    match (base.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{rest}"),
    }
}

#[cfg(test)]
mod tests {
    /// Naming a credential file opts back into it, exactly as for a pruned
    /// directory — and a broad pattern still does not.
    #[test]
    fn a_pattern_that_names_a_credential_file_gets_it() {
        assert!(super::hidden_credential(".env", "**/*"));
        assert!(super::hidden_credential(".env", "src/**/*.rs"));
        assert!(!super::hidden_credential(".env", "**/.env"));
        assert!(!super::hidden_credential(".env", ".env"));
        // A key is still a key when the pattern is about keys generally.
        assert!(super::hidden_credential("server.pem", "**/*.pem"));
    }

    /// The credential-file rule, which `find` and `grep` both ride on.
    #[test]
    fn credential_files_are_recognised() {
        for name in [
            ".env",
            ".ENV",
            ".env.production",
            ".envrc",
            ".netrc",
            ".npmrc",
            ".git-credentials",
            "credentials",
            "id_rsa",
            "id_ed25519",
            "server.pem",
            "tls.KEY",
            "bundle.p12",
        ] {
            assert!(super::is_credential_file(name), "{name} holds credentials");
        }
        // Ordinary files a coding agent works on every turn must not be swept up:
        // a rule that catches source code would make the tools useless.
        for name in [
            "main.rs",
            "environment.rs",
            "env.rs",
            "keyboard.ts",
            "Cargo.toml",
            "README.md",
            "monkey.py",
        ] {
            assert!(!super::is_credential_file(name), "{name} is ordinary");
        }
    }

    use super::{join, matches, normalize, truncate, walk, Entry, MAX_OUTPUT_BYTES, MAX_RESULTS};
    use crate::PRUNED_DIRS;

    /// A fake tree: directory path → entries. Root is `""`.
    fn tree<'a>(
        dirs: &'a [(&'a str, &'a [(&'a str, bool)])],
    ) -> impl Fn(&str) -> Option<Vec<Entry>> + 'a {
        move |path: &str| {
            let key = if path == "." { "" } else { path };
            dirs.iter().find(|(d, _)| *d == key).map(|(_, entries)| {
                entries
                    .iter()
                    .map(|(name, is_dir)| Entry {
                        name: (*name).to_string(),
                        is_dir: *is_dir,
                    })
                    .collect()
            })
        }
    }

    #[test]
    fn star_stays_within_one_segment() {
        assert!(matches("*.rs", "lib.rs"));
        assert!(!matches("*.rs", "src/lib.rs"));
        assert!(matches("src/*.rs", "src/lib.rs"));
    }

    #[test]
    fn double_star_spans_segments_including_none() {
        assert!(matches("**/*.rs", "lib.rs"));
        assert!(matches("**/*.rs", "a/b/c/lib.rs"));
        assert!(matches("src/**/mod.rs", "src/mod.rs"));
        assert!(matches("src/**/mod.rs", "src/a/b/mod.rs"));
        assert!(!matches("src/**/mod.rs", "other/a/mod.rs"));
    }

    #[test]
    fn question_mark_matches_exactly_one_char() {
        assert!(matches("v?.txt", "v1.txt"));
        assert!(!matches("v?.txt", "v10.txt"));
    }

    #[test]
    fn backtracking_star_matches_repeated_prefixes() {
        // Naive greedy matching fails this; the `*` must give chars back.
        assert!(matches("*abc", "aababc"));
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(!matches("a*b*c", "axxbyy"));
    }

    #[test]
    fn slashless_patterns_become_recursive() {
        assert_eq!(normalize("*.rs"), "**/*.rs");
        assert_eq!(normalize("src/*.rs"), "src/*.rs");
    }

    #[test]
    fn walk_finds_matches_across_the_tree_sorted() {
        let list = tree(&[
            ("", &[("src", true), ("README.md", false)]),
            ("src", &[("main.rs", false), ("util", true)]),
            ("src/util", &[("helper.rs", false), ("notes.md", false)]),
        ]);
        let found = walk(".", "*.rs", &list);
        assert_eq!(found.paths, vec!["src/main.rs", "src/util/helper.rs"]);
        assert!(found.bounded_by.is_none());
    }

    #[test]
    fn walk_reports_paths_relative_to_the_workspace_not_the_root() {
        let list = tree(&[
            ("src", &[("main.rs", false), ("util", true)]),
            ("src/util", &[("helper.rs", false)]),
        ]);
        // Pattern is matched relative to `src`, results are prefixed with it.
        let found = walk("src", "*.rs", &list);
        assert_eq!(found.paths, vec!["src/main.rs", "src/util/helper.rs"]);
    }

    #[test]
    fn heavy_dirs_are_pruned_unless_named() {
        let dirs: Vec<(&str, &[(&str, bool)])> = vec![
            ("", &[("target", true), ("src", true)]),
            ("target", &[("build.rs", false)]),
            ("src", &[("main.rs", false)]),
        ];
        let list = tree(&dirs);
        assert!(PRUNED_DIRS.contains(&"target"));
        assert_eq!(walk(".", "**/*.rs", &list).paths, vec!["src/main.rs"]);
        // Naming the pruned directory opts back in.
        assert_eq!(
            walk(".", "target/**/*.rs", &list).paths,
            vec!["target/build.rs"]
        );
    }

    #[test]
    fn an_unreadable_directory_is_skipped_not_fatal() {
        let list = tree(&[("", &[("ghost", true), ("a.rs", false)])]);
        // "ghost" has no entry in the fake tree → listing it returns None.
        assert_eq!(walk(".", "*.rs", &list).paths, vec!["a.rs"]);
    }

    #[test]
    fn the_result_cap_bounds_the_walk_and_is_reported() {
        let many: Vec<(String, bool)> = (0..MAX_RESULTS + 50)
            .map(|i| (format!("f{i}.rs"), false))
            .collect();
        let list = |path: &str| {
            (path == "." || path.is_empty()).then(|| {
                many.iter()
                    .map(|(name, is_dir)| Entry {
                        name: name.clone(),
                        is_dir: *is_dir,
                    })
                    .collect()
            })
        };
        let found = walk(".", "*.rs", &list);
        assert_eq!(found.paths.len(), MAX_RESULTS);
        assert_eq!(found.bounded_by, Some("result cap"));
    }

    #[test]
    fn joining_treats_dot_and_empty_as_here() {
        assert_eq!(join(".", "src"), "src");
        assert_eq!(join("", "src"), "src");
        assert_eq!(join("src", ""), "src");
        assert_eq!(join("src", "lib.rs"), "src/lib.rs");
    }

    #[test]
    fn output_under_cap_passes_through() {
        let small = "1:hello\n2:world".to_string();
        assert_eq!(truncate(small.clone()), small);
    }

    #[test]
    fn output_over_cap_is_truncated_with_marker() {
        let big = "x".repeat(MAX_OUTPUT_BYTES + 100);
        let result = truncate(big);
        assert!(result.len() < MAX_OUTPUT_BYTES + 200);
        assert!(
            result.contains("…[truncated:"),
            "truncation marker must be present"
        );
    }

    #[test]
    fn truncation_at_multibyte_boundary_does_not_panic() {
        // A 2-byte char ('é') repeated so the cap lands mid-character.
        let big = "é".repeat(MAX_OUTPUT_BYTES);
        let result = truncate(big);
        assert!(
            result.contains("…[truncated:"),
            "truncation marker must be present"
        );
    }
}
