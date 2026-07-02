//! Host-side `host-process` backend (Phase 7 Slice 7b) — **bounded** command
//! execution.
//!
//! This is code execution, so the mediation is the point: **default-deny**
//! (disabled unless a workspace is configured), the working directory is **jailed
//! to the workspace** (reusing [`Workspace::resolve`]), and each run has a
//! **timeout** and an **output cap**. v1 runs to completion by polling `try_wait`
//! and killing on timeout.
//!
//! v1 caveats: output is read after the child exits, so a command that fills the
//! OS pipe buffer (very large output) before exiting could block — bounded by the
//! timeout. Long-lived / streaming children are a later refinement.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::host_fs::Workspace;

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
}

impl ProcessRunner {
    /// A runner that denies every exec (no workspace configured).
    #[must_use]
    pub const fn disabled() -> Self {
        Self { workspace: None, timeout: Duration::from_secs(0), output_cap: 0 }
    }

    /// A runner rooted at `workspace`, with a per-command `timeout` and `output_cap`
    /// (bytes) applied to each captured stream.
    #[must_use]
    pub const fn new(workspace: Workspace, timeout: Duration, output_cap: usize) -> Self {
        Self { workspace: Some(workspace), timeout, output_cap }
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
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::open(&dir).unwrap();
        let runner = ProcessRunner::new(ws.clone(), Duration::from_secs(5), 64 * 1024);
        (ws, runner)
    }

    #[test]
    fn runs_a_command_and_captures_stdout() {
        let (_ws, runner) = runner();
        let exit = runner.exec("echo", &["hello".to_string()], None, None).unwrap();
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
        assert_eq!(runner.exec("echo", &["x".to_string()], None, None), Err(ProcError::Denied));
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
        assert_eq!(runner.exec("sleep", &["5".to_string()], None, None), Err(ProcError::Timeout));
    }

    #[test]
    fn output_is_capped() {
        let (ws, _) = runner();
        let runner = ProcessRunner::new(ws, Duration::from_secs(5), 4);
        let exit = runner.exec("echo", &["abcdefghij".to_string()], None, None).unwrap();
        assert!(exit.stdout.starts_with("abcd"), "capped: {:?}", exit.stdout);
        assert!(exit.stdout.contains("truncated"));
    }
}
