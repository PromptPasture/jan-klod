//! Host-side `host-process` backend — bounded command execution.
//!
//! This is code execution, so mediation is the point: default-deny (disabled
//! unless a workspace is configured), cwd jailed to the workspace (reusing
//! [`Workspace::resolve`]), and each run bounded by a timeout and output cap.
//! Runs to completion by polling `try_wait`, killing on timeout.
//!
//! Caveat: output is read after the child exits, so a command that fills the OS
//! pipe buffer before exiting could block — bounded by the timeout. Streaming
//! children are unsupported.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::host_fs::Workspace;

/// The only environment variables a child process inherits by default.
///
/// The environment is cleared and rebuilt from this allowlist instead of
/// inherited wholesale: the host's env holds secrets (`OPENAI_API_KEY`,
/// `JAN_KLOD_TOKEN`), and a plain `env` through `tool-shell` would put them in
/// tool output, then the transcript, then the next request to the model. Each
/// entry below is a name a coding agent's commands actually need:
///
/// - `PATH` — command resolution.
/// - `HOME` — git/cargo config.
/// - `CARGO_HOME`, `RUSTUP_HOME` — toolchain installed elsewhere.
/// - `TMPDIR` — tools assume one exists.
/// - `LANG`, `LC_ALL`, `LC_CTYPE` — text encoding, so output isn't mojibake.
///
/// `TERM` is deliberately excluded: ANSI colour escapes in tool output are
/// context the model pays for and cannot use.
pub const BASE_ENV: [&str; 8] = [
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

/// Why an exec failed (mirrors `host-process.proc-error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcError {
    /// Execution is disabled, or the `cwd` escapes the workspace.
    Denied,
    /// The command exceeded its time budget and was killed.
    Timeout,
    /// The command could not be spawned.
    SpawnFailed,
}

/// The outcome of a completed command (mirrors `host-process.exit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Exit code, or -1 if terminated by a signal.
    pub code: i32,
    /// Captured stdout (truncated to the output cap).
    pub stdout: String,
    /// Captured stderr (truncated to the output cap).
    pub stderr: String,
}

/// Runs commands under a workspace, bounded by a timeout and output cap.
/// Default-deny: [`ProcessRunner::disabled`] rejects every exec.
#[derive(Clone)]
pub struct ProcessRunner {
    workspace: Option<Workspace>,
    timeout: Duration,
    output_cap: usize,
    /// Extra environment names the operator granted (see [`BASE_ENV`]).
    env_passthrough: Vec<String>,
}

impl ProcessRunner {
    /// A runner that denies every exec (no workspace configured).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            workspace: None,
            timeout: Duration::from_secs(0),
            output_cap: 0,
            env_passthrough: Vec::new(),
        }
    }

    /// A runner rooted at `workspace`, with a per-command `timeout` and `output_cap`
    /// (bytes) applied to each captured stream.
    #[must_use]
    pub const fn new(workspace: Workspace, timeout: Duration, output_cap: usize) -> Self {
        Self {
            workspace: Some(workspace),
            timeout,
            output_cap,
            env_passthrough: Vec::new(),
        }
    }

    /// Additionally pass these environment variables through to child processes.
    ///
    /// A grant, one name at a time, like `network.allow`. For the command that
    /// genuinely needs `GITHUB_TOKEN` — and nothing else.
    #[must_use]
    pub fn with_env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// The environment a child process gets: [`BASE_ENV`] plus whatever the
    /// operator granted, and nothing else.
    fn environment(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = BASE_ENV
            .iter()
            .filter_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| ((*name).to_string(), value))
            })
            .collect();
        for name in &self.env_passthrough {
            if let Ok(value) = std::env::var(name) {
                out.push((name.clone(), value));
            }
        }
        // Not inherited, deliberately set: without `TERM` most tools already drop
        // colour, and this makes it explicit. ANSI escapes in tool output are
        // context the model pays for and cannot use.
        out.push(("NO_COLOR".to_string(), "1".to_string()));
        out
    }

    /// Run `command` with `args`, an optional workspace-relative `cwd`, and optional
    /// `stdin`.
    ///
    /// # Errors
    /// [`ProcError::Denied`] (disabled or `cwd` escape), [`ProcError::SpawnFailed`],
    /// or [`ProcError::Timeout`].
    pub fn exec(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        stdin: Option<&str>,
    ) -> Result<Exit, ProcError> {
        let Some(workspace) = &self.workspace else {
            return Err(ProcError::Denied);
        };
        let dir = match cwd {
            Some(rel) => workspace.resolve(rel).map_err(|_| ProcError::Denied)?,
            None => workspace.root().to_path_buf(),
        };

        let mut child = Command::new(command)
            .args(args)
            .current_dir(&dir)
            .env_clear()
            .envs(self.environment())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| ProcError::SpawnFailed)?;

        // Feed stdin (if any) and close it so a reader doesn't hang.
        if let Some(mut handle) = child.stdin.take() {
            if let Some(input) = stdin {
                let _ = handle.write_all(input.as_bytes());
            }
        } // handle dropped here -> stdin closed

        // Poll to completion, killing on timeout.
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(ProcError::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return Err(ProcError::SpawnFailed),
            }
        };

        Ok(Exit {
            code: status.code().unwrap_or(-1),
            stdout: read_capped(child.stdout.take(), self.output_cap),
            stderr: read_capped(child.stderr.take(), self.output_cap),
        })
    }
}

/// Read a captured stream to a UTF-8 string, truncated to `cap` bytes.
fn read_capped(stream: Option<impl Read>, cap: usize) -> String {
    let mut buf = Vec::new();
    if let Some(stream) = stream {
        let _ = stream.take(cap as u64 + 1).read_to_end(&mut buf);
    }
    let truncated = buf.len() > cap;
    buf.truncate(cap);
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        text.push_str("…[truncated]");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> (Workspace, ProcessRunner) {
        let dir = std::env::temp_dir().join(format!(
            "jk-proc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::open(&dir).unwrap();
        let runner = ProcessRunner::new(ws.clone(), Duration::from_secs(5), 64 * 1024);
        (ws, runner)
    }

    /// The environment is a credential store, and this used to hand all of it to
    /// every command the model asked for.
    #[test]
    fn a_command_does_not_inherit_the_hosts_secrets() {
        std::env::set_var("OPENAI_API_KEY", "sk-test-must-not-leak");
        std::env::set_var("JAN_KLOD_TOKEN", "bearer-must-not-leak");
        let (_ws, runner) = runner();
        let exit = runner
            .exec("/bin/sh", &["-c".into(), "env".into()], None, None)
            .expect("sh runs");
        assert!(
            !exit.stdout.contains("must-not-leak"),
            "the provider key and the gateway token reached a subprocess, so `env` \
             puts them in tool output, the transcript, and the next request to the \
             model:\n{}",
            exit.stdout
        );
    }

    /// …but the commands a coding agent exists to run still have to work.
    #[test]
    fn a_command_keeps_what_it_needs_to_run() {
        let (_ws, runner) = runner();
        let exit = runner
            .exec(
                "/bin/sh",
                &["-c".into(), "echo $PATH; echo $HOME".into()],
                None,
                None,
            )
            .expect("sh runs");
        assert!(
            !exit.stdout.trim().is_empty(),
            "PATH and HOME survive: {:?}",
            exit.stdout
        );
        // A program found via PATH, which is the whole point of keeping it.
        let git = runner.exec("git", &["--version".into()], None, None);
        assert!(git.is_ok(), "a PATH lookup still resolves: {git:?}");
    }

    #[test]
    fn runs_a_command_and_captures_stdout() {
        let (_ws, runner) = runner();
        let exit = runner
            .exec("echo", &["hello".to_string()], None, None)
            .unwrap();
        assert_eq!(exit.code, 0);
        assert_eq!(exit.stdout.trim(), "hello");
    }

    #[test]
    fn reports_a_nonzero_exit_code() {
        let (_ws, runner) = runner();
        // `false` exits 1.
        let exit = runner.exec("false", &[], None, None).unwrap();
        assert_eq!(exit.code, 1);
    }

    #[test]
    fn feeds_stdin() {
        let (_ws, runner) = runner();
        let exit = runner.exec("cat", &[], None, Some("piped in")).unwrap();
        assert_eq!(exit.stdout, "piped in");
    }

    #[test]
    fn disabled_runner_denies() {
        let runner = ProcessRunner::disabled();
        assert_eq!(
            runner.exec("echo", &["x".to_string()], None, None),
            Err(ProcError::Denied)
        );
    }

    #[test]
    fn a_cwd_escape_is_denied() {
        let (_ws, runner) = runner();
        assert_eq!(
            runner.exec("echo", &["x".to_string()], Some("../outside"), None),
            Err(ProcError::Denied)
        );
    }

    #[test]
    fn a_slow_command_times_out() {
        let (ws, _) = runner();
        let runner = ProcessRunner::new(ws, Duration::from_millis(150), 1024);
        assert_eq!(
            runner.exec("sleep", &["5".to_string()], None, None),
            Err(ProcError::Timeout)
        );
    }

    #[test]
    fn output_is_capped() {
        let (ws, _) = runner();
        let runner = ProcessRunner::new(ws, Duration::from_secs(5), 4);
        let exit = runner
            .exec("echo", &["abcdefghij".to_string()], None, None)
            .unwrap();
        assert!(exit.stdout.starts_with("abcd"), "capped: {:?}", exit.stdout);
        assert!(exit.stdout.contains("truncated"));
    }
}
