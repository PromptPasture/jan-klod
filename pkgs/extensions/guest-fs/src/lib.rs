//! Shared helpers for `host-fs`-routed guests (`tool-fs`, `tool-find`, `tool-edit`).
//!
//! A library, not a component: no WIT bindings. Guests share this tested logic
//! instead of copying it locally.
//!
//! Enforces policy about cost and shape: output limits, glob matching, walk bounds.

/// Max output bytes per call.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Max paths from a single walk.
pub const MAX_RESULTS: usize = 500;
/// Max directory entries examined per walk.
pub const MAX_VISITS: usize = 20_000;
/// Max directory depth relative to search root.
pub const MAX_DEPTH: usize = 24;

/// Directories pruned unless the pattern names them explicitly.
///
/// Build/vendor trees are pruned (expensive, rarely intended); name them in the
/// pattern to opt back in.
pub const PRUNED_DIRS: [&str; 6] = [
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".jj",
];

/// One directory entry.
pub struct Entry {
    /// File or directory name (not a path).
    pub name: String,
    /// True if directory.
    pub is_dir: bool,
}

/// Walk results and termination reason.
pub struct Outcome {
    /// Matching paths, workspace-relative and sorted.
    pub paths: Vec<String>,
    /// Reason the walk stopped, if bounded before exhausting the tree.
    pub bounded_by: Option<&'static str>,
}

/// Truncate `output` at [`MAX_OUTPUT_BYTES`] on a UTF-8 boundary, appending a marker.
#[must_use]
pub fn truncate(output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    // Backoff to valid UTF-8 boundary to avoid panic on slice.
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

/// Expand slash-less patterns to search recursively.
///
/// `*.rs` in strict glob means "top level only"; most callers mean recursive.
/// Patterns with any `/` are left unchanged.
#[must_use]
pub fn normalize(pattern: &str) -> String {
    if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    }
}

/// Test if `path` matches glob `pattern`.
///
/// `*` and `?` match one segment; `**` matches any segments including none.
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

/// Match one path segment against `*`/`?` pattern.
fn match_name(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = name.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // Last `*` seen and chars it consumed (for backtracking).
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
            // Give `*` one more char and retry.
            eaten += 1;
            ti = eaten;
            pi = s + 1;
        } else {
            return false;
        }
    }
    pat[pi..].iter().all(|c| *c == '*')
}

/// Check if walk should descend into directory `name`.
///
/// Pruned unless the pattern names them explicitly.
fn should_descend(name: &str, pattern: &str) -> bool {
    !PRUNED_DIRS.contains(&name) || pattern.split('/').any(|segment| segment == name)
}

/// Check if credential file should be withheld from walk results.
///
/// Withheld unless named explicitly in the pattern (same as pruned dirs).
/// Broad globs like `**/*` skip them; explicit `**/.env` does not.
/// Names don't disclose secrets; contents are still read-gated.
fn hidden_credential(name: &str, pattern: &str) -> bool {
    is_credential_file(name) && !pattern.split('/').any(|segment| segment == name)
}

/// Test if filename marks it as holding credentials.
///
/// Reads and greps run without asking (read-only allowlist).
/// A grep for "password" in `.env` exposes secrets to transcript.
/// The shared walk skips these; explicit reads by path are still gated.
///
/// Heuristic based on name only; doesn't catch semantic patterns like
/// `config/production.yaml`.
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

/// Walk `root` collecting files matching `pattern`.
///
/// `list` returns None for unreadable dirs (skipped, not fatal).
/// Paths matched relative to `root` and reported with it.
///
/// All dimensions bounded ([`MAX_VISITS`], [`MAX_DEPTH`], [`MAX_RESULTS`]):
/// glob cost is tree-driven, not arg-driven.
pub fn walk(root: &str, pattern: &str, list: &dyn Fn(&str) -> Option<Vec<Entry>>) -> Outcome {
    let pattern = normalize(pattern);
    let mut paths = Vec::new();
    let mut bounded_by = None;
    let mut visits = 0usize;
    // (path relative to root, depth); "" at root.
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

/// Join workspace-relative fragments, treating `""` and `"."` as here.
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
    /// Naming a credential file opts back in; broad patterns don't.
    #[test]
    fn a_pattern_that_names_a_credential_file_gets_it() {
        assert!(super::hidden_credential(".env", "**/*"));
        assert!(super::hidden_credential(".env", "src/**/*.rs"));
        assert!(!super::hidden_credential(".env", "**/.env"));
        assert!(!super::hidden_credential(".env", ".env"));
        // A key is still a key when the pattern is about keys generally.
        assert!(super::hidden_credential("server.pem", "**/*.pem"));
    }

    /// Credential-file rule (shared by `find` and `grep`).
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
        // Ordinary files must not be caught; would break tools.
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

    /// Mock tree: path → entries. Root is `""`.
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
