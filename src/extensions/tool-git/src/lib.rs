//! `tool-git` — **read-only** repository inspection through `host-process`.
//!
//! Exposes `git` tool with ops:
//! - `status` → porcelain status + branch
//! - `diff` → working or staged diff
//! - `log` → one-line history
//! - `show` → one commit
//! - `branch` → local branches
//!
//! Closed op set prevents arbitrary command execution. Only git runs; only the
//! five read-only ops are reachable. No `commit`, `checkout`, `reset`, `push`.
//!
//! Reading a repo is not side-effect-free: Git's config can run code (fsmonitor,
//! hooksPath, diff/textconv drivers). Every invocation disables these paths (HARDENING)
//! so untrusted repos stay safe. Argv construction is pure Rust (unit-tested);
//! glue for wasm32 only.

/// Largest `log` page. Past this the answer is not history, it is a data dump.
const MAX_COUNT: u64 = 200;
/// Default `log` page.
const DEFAULT_COUNT: u64 = 20;
/// Longest accepted `rev`/`path` argument — a bound on argv, not on taste.
const MAX_ARG_LEN: usize = 200;

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod git {
    use crate::{DEFAULT_COUNT, MAX_ARG_LEN, MAX_COUNT};

    /// Flags prepended to every invocation, disabling hooks that let a checkout run code.
    /// Covers: fsmonitor, hooksPath, ext transports, pager, textconv/diff drivers.
    pub const HARDENING: [&str; 8] = [
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "protocol.ext.allow=never",
        "--no-optional-locks",
    ];

    /// What the caller asked for, already extracted from the JSON arguments.
    pub struct Request<'a> {
        /// The operation name (one of the five allowlisted subcommands).
        pub op: &'a str,
        /// Optional pathspec to narrow `diff`/`log`.
        pub path: Option<&'a str>,
        /// Revision for `show`.
        pub rev: Option<&'a str>,
        /// Whether `diff` should read the index rather than the working tree.
        pub staged: bool,
        /// How many commits `log` returns.
        pub count: Option<u64>,
    }

    /// Build the argument vector for `git`, or explain the refusal.
    /// # Errors
    /// Caller-facing message when op is not allowlisted or arguments fail validation.
    pub fn argv(req: &Request) -> Result<Vec<String>, String> {
        let mut out: Vec<String> = HARDENING.iter().map(|s| (*s).to_string()).collect();
        match req.op {
            "status" => out.extend(["status".into(), "--short".into(), "--branch".into()]),
            "branch" => out.extend(["branch".into(), "--list".into(), "--no-color".into()]),
            "diff" => {
                out.extend([
                    "diff".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                ]);
                if req.staged {
                    out.push("--staged".into());
                }
                push_pathspec(&mut out, req.path)?;
            }
            "log" => {
                let count = req.count.unwrap_or(DEFAULT_COUNT).clamp(1, MAX_COUNT);
                out.extend([
                    "log".into(),
                    "--oneline".into(),
                    "--no-decorate".into(),
                    "-n".into(),
                    count.to_string(),
                ]);
                push_pathspec(&mut out, req.path)?;
            }
            "show" => {
                let rev = req.rev.ok_or_else(|| "op=show needs a `rev`".to_string())?;
                out.extend([
                    "show".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                ]);
                out.push(validate(rev, "rev")?);
            }
            other => {
                return Err(format!(
                    "unknown op `{other}` — this tool reads a repository only: \
                     status | diff | log | show | branch"
                ))
            }
        }
        Ok(out)
    }

    /// Append `-- <path>` so git reads it as a pathspec and never as a flag.
    fn push_pathspec(out: &mut Vec<String>, path: Option<&str>) -> Result<(), String> {
        let Some(path) = path else { return Ok(()) };
        let path = validate(path, "path")?;
        if path.starts_with('/') || path.split('/').any(|seg| seg == "..") {
            return Err(format!("path `{path}` must stay inside the repository"));
        }
        out.push("--".into());
        out.push(path);
        Ok(())
    }

    /// Reject arguments that git would read as options, plus anything outside the
    /// characters revisions and paths are actually made of.
    ///
    /// A leading `-` is the whole risk: `rev` and `path` are the only
    /// caller-supplied argv entries, and `--upload-pack=…`-style options are how
    /// a value becomes a command. The charset check also excludes shell
    /// metacharacters, defence-in-depth even though `host-process` never
    /// involves a shell.
    fn validate(value: &str, what: &str) -> Result<String, String> {
        if value.is_empty() || value.len() > MAX_ARG_LEN {
            return Err(format!("{what} must be 1..={MAX_ARG_LEN} characters"));
        }
        if value.starts_with('-') {
            return Err(format!("{what} `{value}` must not start with `-`"));
        }
        if !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-^~@{}".contains(c))
        {
            return Err(format!(
                "{what} `{value}` has characters this tool does not accept"
            ));
        }
        Ok(value.to_string())
    }

    /// Format a finished run for the model.
    ///
    /// A non-zero exit is information (not a repository, unknown revision), so
    /// it comes back as a readable result rather than an opaque tool error the
    /// model cannot act on.
    pub fn render(code: i32, stdout: &str, stderr: &str) -> String {
        let body = if stdout.trim().is_empty() && code == 0 {
            "(no output)".to_string()
        } else {
            stdout.trim_end().to_string()
        };
        if code == 0 {
            return guest_fs::truncate(body);
        }
        let detail = if stderr.trim().is_empty() {
            &body
        } else {
            stderr.trim_end()
        };
        guest_fs::truncate(format!("git exited {code}: {detail}"))
    }

    #[cfg(test)]
    mod tests {
        use super::{argv, render, Request, HARDENING};
        use crate::{DEFAULT_COUNT, MAX_ARG_LEN, MAX_COUNT};

        fn req(op: &str) -> Request<'_> {
            Request {
                op,
                path: None,
                rev: None,
                staged: false,
                count: None,
            }
        }

        /// The arguments after the hardening prefix — what git actually does.
        fn tail(args: &[String]) -> Vec<&str> {
            args[HARDENING.len()..].iter().map(String::as_str).collect()
        }

        #[test]
        fn every_op_is_hardened_against_repo_supplied_code() {
            for op in ["status", "diff", "log", "branch"] {
                let args = argv(&req(op)).expect("op is allowed");
                assert_eq!(
                    &args[..HARDENING.len()],
                    &HARDENING,
                    "{op} must carry the prefix"
                );
            }
            let shown = argv(&Request {
                rev: Some("HEAD"),
                ..req("show")
            })
            .expect("show is allowed");
            assert_eq!(&shown[..HARDENING.len()], &HARDENING);
        }

        #[test]
        fn status_and_branch_take_no_caller_input() {
            assert_eq!(
                tail(&argv(&req("status")).unwrap()),
                ["status", "--short", "--branch"]
            );
            assert_eq!(
                tail(&argv(&req("branch")).unwrap()),
                ["branch", "--list", "--no-color"]
            );
        }

        #[test]
        fn diff_reads_the_index_only_when_asked() {
            assert_eq!(
                tail(&argv(&req("diff")).unwrap()),
                ["diff", "--no-ext-diff", "--no-textconv"]
            );
            let staged = argv(&Request {
                staged: true,
                ..req("diff")
            })
            .unwrap();
            assert!(tail(&staged).contains(&"--staged"));
        }

        #[test]
        fn a_pathspec_is_passed_after_a_separator_so_it_cannot_be_a_flag() {
            let args = argv(&Request {
                path: Some("src/lib.rs"),
                ..req("diff")
            })
            .unwrap();
            let tail = tail(&args);
            assert_eq!(&tail[tail.len() - 2..], ["--", "src/lib.rs"]);
        }

        #[test]
        fn log_defaults_and_clamps_its_page_size() {
            let default = argv(&req("log")).unwrap();
            assert!(tail(&default).contains(&DEFAULT_COUNT.to_string().as_str()));
            let huge = argv(&Request {
                count: Some(10_000),
                ..req("log")
            })
            .unwrap();
            assert!(
                tail(&huge).contains(&MAX_COUNT.to_string().as_str()),
                "page size is clamped"
            );
            let zero = argv(&Request {
                count: Some(0),
                ..req("log")
            })
            .unwrap();
            assert!(
                tail(&zero).contains(&"1"),
                "0 commits is not a useful answer"
            );
        }

        #[test]
        fn mutating_subcommands_are_not_expressible() {
            for op in ["commit", "push", "checkout", "reset", "clean", "config"] {
                let err = argv(&req(op)).expect_err("must be refused");
                assert!(err.contains("reads a repository only"), "{op}: {err}");
            }
        }

        #[test]
        fn an_argument_that_looks_like_an_option_is_refused() {
            // The classic: a "revision" that is really `git show --upload-pack=…`.
            let err = argv(&Request {
                rev: Some("--upload-pack=sh"),
                ..req("show")
            })
            .unwrap_err();
            assert!(err.contains("must not start with `-`"), "{err}");
            let err = argv(&Request {
                path: Some("--output=/tmp/x"),
                ..req("log")
            })
            .unwrap_err();
            assert!(err.contains("must not start with `-`"), "{err}");
        }

        #[test]
        fn shell_metacharacters_are_refused_even_though_no_shell_is_involved() {
            for bad in ["a;rm -rf /", "$(id)", "`id`", "a|b", "x&y"] {
                assert!(
                    argv(&Request {
                        rev: Some(bad),
                        ..req("show")
                    })
                    .is_err(),
                    "{bad}"
                );
            }
        }

        #[test]
        fn a_pathspec_cannot_leave_the_repository() {
            for bad in ["/etc/passwd", "../secrets", "a/../../b"] {
                let err = argv(&Request {
                    path: Some(bad),
                    ..req("diff")
                })
                .unwrap_err();
                assert!(err.contains("inside the repository"), "{bad}: {err}");
            }
        }

        #[test]
        fn over_long_arguments_are_refused() {
            let long = "a".repeat(MAX_ARG_LEN + 1);
            assert!(argv(&Request {
                rev: Some(&long),
                ..req("show")
            })
            .is_err());
        }

        #[test]
        fn show_without_a_revision_says_what_is_missing() {
            let err = argv(&req("show")).unwrap_err();
            assert!(err.contains("needs a `rev`"), "{err}");
        }

        #[test]
        fn ordinary_revisions_pass() {
            for good in [
                "HEAD",
                "HEAD~3",
                "main",
                "origin/main",
                "a1b2c3d",
                "v1.0.0",
                "HEAD@{1}",
            ] {
                let args = argv(&Request {
                    rev: Some(good),
                    ..req("show")
                })
                .unwrap_or_else(|e| panic!("{good} should be accepted: {e}"));
                assert!(args.contains(&good.to_string()));
            }
        }

        #[test]
        fn a_failed_run_returns_readable_information_not_an_opaque_error() {
            let out = render(128, "", "fatal: not a git repository");
            assert!(out.contains("git exited 128"), "{out}");
            assert!(out.contains("not a git repository"), "{out}");
        }

        #[test]
        fn a_clean_repository_says_so_rather_than_returning_nothing() {
            assert_eq!(render(0, "", ""), "(no output)");
            assert_eq!(render(0, " M src/lib.rs\n", ""), " M src/lib.rs");
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::git;

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod bindings {
        wit_bindgen::generate!({ world: "tool-world", path: "../../../wit" });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_process;

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            Ok(())
        }
        fn start() -> Result<(), String> {
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "git".to_string(),
                description: "Inspect the workspace's git repository (read-only). \
                    Choose the operation with `op`: status | diff | log | show | branch. \
                    Nothing here modifies the repository."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": ["status", "diff", "log", "show", "branch"]
                        },
                        "path": {
                            "type": "string",
                            "description": "for op=diff|log: limit to this file or directory"
                        },
                        "rev": {
                            "type": "string",
                            "description": "for op=show: a revision, e.g. `HEAD` or a sha"
                        },
                        "staged": {
                            "type": "boolean",
                            "description": "for op=diff: show the staged diff instead"
                        },
                        "count": {
                            "type": "integer",
                            "description": "for op=log: how many commits (default 20)"
                        }
                    },
                    "required": ["op"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let op = value
                .get("op")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;

            let request = git::Request {
                op,
                path: value.get("path").and_then(serde_json::Value::as_str),
                rev: value.get("rev").and_then(serde_json::Value::as_str),
                staged: value
                    .get("staged")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                count: value.get("count").and_then(serde_json::Value::as_u64),
            };

            // A refused argument is a recoverable, self-correctable answer — the
            // model is told what is wrong, not just that something was.
            let args = match git::argv(&request) {
                Ok(args) => args,
                Err(message) => return Ok(format!("REFUSED: {message}")),
            };

            let exit = host_process::exec("git", &args, None, None)
                .map_err(|_| ToolError::ExecutionFailed)?;
            Ok(git::render(exit.code, &exit.stdout, &exit.stderr))
        }
    }

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
