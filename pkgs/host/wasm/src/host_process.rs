//! Bounded command execution for `host-process`: cwd jailed, timeout and output cap; output is read after exit.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::host_fs::Workspace;
use crate::sandbox::{SandboxBackend, SandboxPolicy};

/// Env vars a child inherits; the rest of the host env (secrets) is cleared and `TMPDIR` is overridden.
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

/// Runs bounded commands under a workspace; default-deny via [`ProcessRunner::disabled`].
#[derive(Clone)]
pub struct ProcessRunner {
    workspace: Option<Workspace>,
    timeout: Duration,
    output_cap: usize,
    env_passthrough: Vec<String>,
    /// `None` is approval-only; boot sets this only after reporting the mode as `Os`.
    confinement: Option<Confinement>,
    long_lived: Vec<LongLived>,
}

/// A long-lived child an operator named in `execution.long-lived`; a guest can only start what is written down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongLived {
    /// What a guest asks for.
    pub name: String,
    /// The program to run. Never supplied by the guest.
    pub command: String,
    /// Its arguments. Also never supplied by the guest.
    pub args: Vec<String>,
}

#[derive(Clone)]
struct Confinement {
    backend: std::sync::Arc<dyn SandboxBackend>,
    policy: SandboxPolicy,
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
            confinement: None,
            long_lived: Vec::new(),
        }
    }

    /// A runner rooted at `workspace`, with per-command `timeout` and `output_cap` (bytes) per captured stream.
    #[must_use]
    pub const fn new(workspace: Workspace, timeout: Duration, output_cap: usize) -> Self {
        Self {
            workspace: Some(workspace),
            timeout,
            output_cap,
            env_passthrough: Vec::new(),
            confinement: None,
            long_lived: Vec::new(),
        }
    }

    /// Confine every command with `backend` under `policy`; boot calls this only for effective mode `Os`.
    #[must_use]
    pub fn with_sandbox(
        mut self,
        backend: std::sync::Arc<dyn SandboxBackend>,
        policy: SandboxPolicy,
    ) -> Self {
        self.confinement = Some(Confinement { backend, policy });
        self
    }

    /// Pass these environment variables through to children (a grant per name).
    #[must_use]
    pub fn with_env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// Permit these, and only these, long-lived children.
    #[must_use]
    pub fn with_long_lived(mut self, children: Vec<LongLived>) -> Self {
        self.long_lived = children;
        self
    }

    /// The names an operator granted, in configured order; what a name runs stays here.
    #[must_use]
    pub fn long_lived_names(&self) -> Vec<String> {
        self.long_lived
            .iter()
            .map(|child| child.name.clone())
            .collect()
    }

    /// The entire admission decision: no path turns a guest string into a program.
    #[must_use]
    pub fn long_lived_grant(&self, name: &str) -> Option<&LongLived> {
        self.long_lived.iter().find(|child| child.name == name)
    }

    /// `TMPDIR` is overridden, see [`Self::scratch_dir`].
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
        if let Some(scratch) = self.scratch_dir() {
            out.retain(|(name, _)| name != "TMPDIR");
            out.push(("TMPDIR".to_string(), scratch));
        }
        out.push(("NO_COLOR".to_string(), "1".to_string()));
        out
    }

    /// Not the host's `TMPDIR`: a confined command may write only under the workspace, so an inherited path would be denied.
    fn scratch_dir(&self) -> Option<String> {
        let root = self
            .workspace
            .as_ref()?
            .root()
            .join(".jan-klod")
            .join("tmp");
        std::fs::create_dir_all(&root).ok()?;
        root.to_str().map(str::to_owned)
    }

    /// Confined before configured: a backend carries only program and arguments, so cwd, env and pipes are applied afterwards.
    fn prepared(
        &self,
        command: &str,
        args: &[String],
        dir: &std::path::Path,
    ) -> Result<Command, ProcError> {
        let mut base = Command::new(command);
        base.args(args);
        let mut spawnable = match &self.confinement {
            None => base,
            Some(confinement) => confinement
                .backend
                .confine(base, &confinement.policy)
                .map_err(|err| {
                    eprintln!(
                        "WARN [core] {} refused to confine `{command}` ({err:?}); the command \
                         is denied rather than run unconfined",
                        confinement.backend.name()
                    );
                    ProcError::Denied
                })?,
        };
        spawnable
            .current_dir(dir)
            .env_clear()
            .envs(self.environment())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(spawnable)
    }

    /// Start the long-lived child granted under `name`; command and args come from the grant, never the caller.
    ///
    /// # Errors
    /// [`ProcError::Denied`] with no grant, no workspace or a refused confinement; [`ProcError::SpawnFailed`] if it cannot start.
    pub fn spawn_long_lived(&self, name: &str) -> Result<LiveChild, ProcError> {
        let Some(workspace) = &self.workspace else {
            return Err(ProcError::Denied);
        };
        let Some(grant) = self.long_lived_grant(name) else {
            eprintln!(
                "WARN [core] host-process: no long-lived child named `{name}` — \
                 add it to `execution.long-lived` to grant it"
            );
            return Err(ProcError::Denied);
        };
        let child = self
            .prepared(&grant.command, &grant.args, workspace.root())
            .and_then(|mut c| c.spawn().map_err(|_| ProcError::SpawnFailed))?;
        Ok(LiveChild::new(child, self.output_cap, name.to_string()))
    }

    /// Run `command` with `args`, an optional workspace-relative `cwd` and optional `stdin`.
    ///
    /// # Errors
    /// [`ProcError::Denied`] (disabled or `cwd` escape), [`ProcError::SpawnFailed`], or [`ProcError::Timeout`].
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

        let mut child = self
            .prepared(command, args, &dir)?
            .spawn()
            .map_err(|_| ProcError::SpawnFailed)?;

        if let Some(mut handle) = child.stdin.take() {
            if let Some(input) = stdin {
                let _ = handle.write_all(input.as_bytes());
            }
        }

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

const STDERR_DRAIN: Duration = Duration::from_millis(200);

/// A long-lived child the host holds open; reader threads keep a silent child from hanging the turn thread.
pub struct LiveChild {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    /// Bounded by the output cap, oldest dropped; an undrained pipe would stop the child.
    stdout: std::sync::Arc<(
        std::sync::Mutex<std::collections::VecDeque<u8>>,
        std::sync::Condvar,
    )>,
    /// A channel, not a buffer: a child writes then exits, so a buffer read on exit detection can be empty.
    stderr: std::sync::mpsc::Receiver<String>,
    cap: usize,
    name: String,
    /// Without this, exit is logged on every `is_running` poll.
    exit_reported: bool,
}

impl LiveChild {
    fn new(mut child: std::process::Child, cap: usize, name: String) -> Self {
        let stdin = child.stdin.take();
        let stdout = std::sync::Arc::new((
            std::sync::Mutex::new(std::collections::VecDeque::new()),
            std::sync::Condvar::new(),
        ));
        if let Some(mut out) = child.stdout.take() {
            let buffer = std::sync::Arc::clone(&stdout);
            let child_name = name.clone();
            std::thread::spawn(move || {
                let mut buf = [0_u8; 8192];
                let mut dropped_any = false;
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let (lock, arrived) = &*buffer;
                    let Ok(mut unread) = lock.lock() else {
                        break;
                    };
                    unread.extend(&buf[..n]);
                    if unread.len() > cap {
                        let over = unread.len() - cap;
                        unread.drain(..over);
                        if !dropped_any {
                            dropped_any = true;
                            eprintln!(
                                "WARN [core] host-process: `{child_name}` has \
                                 printed more than the {cap}-byte output cap \
                                 without being read; the oldest output is \
                                 being dropped"
                            );
                        }
                    }
                    drop(unread);
                    arrived.notify_all();
                }
            });
        }
        let (tx_err, stderr) = std::sync::mpsc::channel();
        if let Some(mut err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut tail: Vec<u8> = Vec::new();
                let mut buf = [0_u8; 8192];
                while let Ok(n) = err.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    tail.extend_from_slice(&buf[..n]);
                    if tail.len() > cap {
                        tail.drain(..tail.len() - cap);
                    }
                }
                let _ = tx_err.send(String::from_utf8_lossy(&tail).into_owned());
            });
        }
        Self {
            child,
            stdin,
            stdout,
            stderr,
            cap,
            name,
            exit_reported: false,
        }
    }

    /// Write to the child's stdin.
    ///
    /// # Errors
    /// [`ProcError::SpawnFailed`] if the pipe is gone or the write fails.
    pub fn write_stdin(&mut self, data: &str) -> Result<(), ProcError> {
        let Some(pipe) = self.stdin.as_mut() else {
            return Err(ProcError::SpawnFailed);
        };
        pipe.write_all(data.as_bytes())
            .and_then(|()| pipe.flush())
            .map_err(|_| ProcError::SpawnFailed)
    }

    /// Read at most `max_bytes`, waiting up to `timeout`; empty means nothing arrived, not that the child finished.
    pub fn read_stdout(&mut self, max_bytes: usize, timeout: Duration) -> String {
        let (lock, arrived) = &*self.stdout;
        let Ok(unread) = lock.lock() else {
            return String::new();
        };
        let Ok((mut unread, _)) = arrived.wait_timeout_while(unread, timeout, |u| u.is_empty())
        else {
            return String::new();
        };
        let take = max_bytes.min(self.cap).min(unread.len());
        let taken: Vec<u8> = unread.drain(..take).collect();
        String::from_utf8_lossy(&taken).into_owned()
    }

    /// Whether the child is still alive; the first observed exit goes to the host log.
    pub fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                if !self.exit_reported {
                    self.exit_reported = true;
                    for line in self.death_report(status) {
                        eprintln!("{line}");
                    }
                }
                false
            }
            Err(_) => false,
        }
    }

    fn death_report(&self, status: std::process::ExitStatus) -> Vec<String> {
        let mut lines = vec![format!(
            "WARN [core] host-process: long-lived child `{}` {}",
            self.name,
            describe_exit(status)
        )];
        let tail = self.stderr_tail();
        let tail = tail.trim();
        if !tail.is_empty() {
            lines.push(format!(
                "WARN [core] host-process: long-lived child `{}` last stderr: {tail}",
                self.name
            ));
        }
        lines
    }

    /// Consuming: the thread sends one message.
    fn stderr_tail(&self) -> String {
        self.stderr.recv_timeout(STDERR_DRAIN).unwrap_or_default()
    }

    /// Kill it and reap it. Idempotent.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LiveChild {
    /// A child must not outlive its instance; this drop runs however the instance goes.
    fn drop(&mut self) {
        self.kill();
    }
}

/// The long-lived children one extension instance holds; dropping this kills every child in it.
#[derive(Default)]
pub struct Children {
    live: std::collections::HashMap<u32, LiveChild>,
    /// Monotonic, so a killed handle is never reissued and a stale one cannot address another's child.
    next: u32,
}

impl Children {
    /// Start the child `runner` grants under `name`.
    ///
    /// # Errors
    /// Whatever [`ProcessRunner::spawn_long_lived`] refuses with.
    pub fn spawn(&mut self, runner: &ProcessRunner, name: &str) -> Result<u32, ProcError> {
        let live = runner.spawn_long_lived(name)?;
        self.next += 1;
        let handle = self.next;
        self.live.insert(handle, live);
        Ok(handle)
    }

    /// Write to a child's stdin.
    ///
    /// # Errors
    /// [`ProcError::Denied`] for a handle this instance was not given.
    pub fn write_stdin(&mut self, handle: u32, data: &str) -> Result<(), ProcError> {
        self.live
            .get_mut(&handle)
            .ok_or(ProcError::Denied)?
            .write_stdin(data)
    }

    /// Read from a child's stdout; see [`LiveChild::read_stdout`].
    ///
    /// # Errors
    /// [`ProcError::Denied`] for an unknown handle.
    pub fn read_stdout(
        &mut self,
        handle: u32,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<String, ProcError> {
        Ok(self
            .live
            .get_mut(&handle)
            .ok_or(ProcError::Denied)?
            .read_stdout(max_bytes, timeout))
    }

    /// Whether a child is alive; `false` for an unknown handle so pollers terminate.
    pub fn is_running(&mut self, handle: u32) -> bool {
        self.live
            .get_mut(&handle)
            .is_some_and(LiveChild::is_running)
    }

    /// Kill and forget: removing the [`LiveChild`] from the map kills it.
    pub fn kill(&mut self, handle: u32) {
        self.live.remove(&handle);
    }
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("was killed by signal {signal}");
        }
    }
    status.code().map_or_else(
        || "exited with no status".to_string(),
        |code| format!("exited with code {code}"),
    )
}

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

    #[test]
    fn a_name_no_grant_carries_is_refused_by_the_shared_table() {
        let mut children = Children::default();
        let ungranted = ProcessRunner::disabled();
        assert_eq!(
            children.spawn(&ungranted, "nothing-named-this"),
            Err(ProcError::Denied)
        );
        assert_eq!(
            children.write_stdin(41, "x"),
            Err(ProcError::Denied),
            "a handle this table never issued reaches nobody"
        );
        assert!(!children.is_running(41));
    }

    #[test]
    fn an_exit_is_described_by_code_and_a_kill_by_signal() {
        let clean = Command::new("sh")
            .args(["-c", "exit 0"])
            .output()
            .expect("sh runs")
            .status;
        assert_eq!(describe_exit(clean), "exited with code 0");

        let failed = Command::new("sh")
            .args(["-c", "exit 3"])
            .output()
            .expect("sh runs")
            .status;
        assert_eq!(describe_exit(failed), "exited with code 3");

        #[cfg(unix)]
        {
            let mut child = Command::new("sh")
                .args(["-c", "sleep 30"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("sh spawns");
            child.kill().expect("the child is killable");
            let status = child.wait().expect("the child is reapable");
            assert_eq!(
                describe_exit(status),
                "was killed by signal 9",
                "a signalled child has no exit code, and saying so is the point"
            );
        }
    }

    #[test]
    fn the_four_deaths_are_four_different_reports() {
        let (ws, _) = runner();
        let rows = [
            (
                "bad-arguments",
                "echo \"error: Unrecognized option: 'writable'\" >&2; exit 2",
                "Unrecognized option",
            ),
            ("missing-command", "no-such-command-xyz", "127"),
            ("syntax-error", "if", "syntax error"),
            ("clean-exit", "exit 0", "code 0"),
        ];

        let runner = ProcessRunner::new(ws, Duration::from_secs(5), 64 * 1024).with_long_lived(
            rows.iter()
                .map(|(name, script, _)| LongLived {
                    name: (*name).to_string(),
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), (*script).to_string()],
                })
                .collect(),
        );

        let mut reports: Vec<(&str, String)> = Vec::new();
        for (name, _, marker) in rows {
            let mut child = runner.spawn_long_lived(name).expect("it starts");
            let status = child.child.wait().expect("it is reapable");
            let report = child.death_report(status).join("\n");
            assert!(
                report.to_lowercase().contains(&marker.to_lowercase()),
                "{name}: the log does not carry what makes this death that death \
                 — wanted {marker:?} (case-insensitively), got:\n{report}"
            );
            reports.push((name, report));
        }

        for (i, (name, report)) in reports.iter().enumerate() {
            for (other_name, other) in &reports[i + 1..] {
                assert_ne!(
                    report, other,
                    "`{name}` and `{other_name}` read identically — which is the \
                     whole defect this issue is about"
                );
            }
        }

        let status_of = |name: &str| {
            let report = &reports
                .iter()
                .find(|(n, _)| *n == name)
                .expect("the row ran")
                .1;
            let head = report.lines().next().expect("a status line");
            head.rsplit("` ").next().expect("a status").to_string()
        };
        assert_eq!(
            status_of("bad-arguments"),
            status_of("syntax-error"),
            "this test is only meaningful while these two share a status — if a \
             shell changes its exit code, pick another colliding pair rather than \
             deleting the assertion"
        );

        let clean = &reports
            .iter()
            .find(|(name, _)| *name == "clean-exit")
            .expect("the clean row ran")
            .1;
        assert!(
            !clean.contains("last stderr"),
            "a silent exit should not claim last words: {clean}"
        );
    }

    #[test]
    fn an_unread_childs_stdout_is_bounded_by_the_output_cap() {
        let cap = 512;
        let (ws, _) = runner();
        let runner =
            ProcessRunner::new(ws, Duration::from_secs(5), cap).with_long_lived(vec![LongLived {
                name: "floods".to_string(),
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "i=0; while [ $i -lt 2000 ]; do echo padding-padding-padding; \
                     i=$((i+1)); done; echo THE-LATEST-LINE; exec sleep 30"
                        .to_string(),
                ],
            }]);

        let mut floods = runner.spawn_long_lived("floods").expect("it starts");
        std::thread::sleep(Duration::from_millis(400));

        let mut drained = String::new();
        loop {
            let chunk = floods.read_stdout(cap, Duration::from_millis(50));
            if chunk.is_empty() {
                break;
            }
            drained.push_str(&chunk);
        }
        floods.kill();

        assert!(
            drained.len() <= cap,
            "an unread child held {} bytes against a {cap}-byte cap; the \
             buffer is unbounded again",
            drained.len()
        );
        assert!(
            drained.contains("THE-LATEST-LINE"),
            "the *last* cap bytes: a caller polling a server wants what it \
             just printed, not its first words: {drained}"
        );
    }

    #[test]
    fn a_dead_childs_last_stderr_is_kept_and_bounded_by_the_output_cap() {
        let cap = 256;
        let (ws, _) = runner();
        let runner = ProcessRunner::new(ws, Duration::from_secs(5), cap).with_long_lived(vec![
            LongLived {
                name: "complains".to_string(),
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "echo \"error: Unrecognized option: 'writable'\" >&2; exit 2".to_string(),
                ],
            },
            LongLived {
                name: "floods".to_string(),
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "i=0; while [ $i -lt 400 ]; do echo padding-padding-padding >&2; \
                     i=$((i+1)); done; echo THE-LAST-WORD >&2; exit 1"
                        .to_string(),
                ],
            },
        ]);

        let mut complains = runner.spawn_long_lived("complains").expect("it starts");
        let _ = complains.child.wait();
        assert!(
            complains.stderr_tail().contains("Unrecognized option"),
            "the name of the bug, in the failing process's own words, is exactly \
             what #132 spent eight runs not having"
        );

        let mut floods = runner.spawn_long_lived("floods").expect("it starts");
        let _ = floods.child.wait();
        let tail = floods.stderr_tail();
        assert!(
            tail.len() <= cap,
            "an unbounded reader on a long-lived child is a memory leak with a \
             good excuse; got {} bytes against a {cap}-byte cap",
            tail.len()
        );
        assert!(
            tail.contains("THE-LAST-WORD"),
            "the *last* cap bytes: a process explains itself on the way out"
        );
    }

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

    #[test]
    fn a_commands_temp_directory_is_inside_the_workspace() {
        let (ws, runner) = runner();
        let exit = runner
            .exec(
                "/bin/sh",
                &["-c".into(), "printf %s \"$TMPDIR\"".into()],
                None,
                None,
            )
            .expect("sh runs");
        let seen = std::path::Path::new(exit.stdout.trim());
        assert!(
            seen.starts_with(ws.root()),
            "TMPDIR {} is not under the workspace {}",
            seen.display(),
            ws.root().display()
        );
        assert!(seen.exists(), "and it was created: {}", seen.display());
    }

    #[test]
    fn a_command_writing_to_its_temp_directory_stays_in_the_workspace() {
        let (ws, runner) = runner();
        let exit = runner
            .exec(
                "/bin/sh",
                &["-c".into(), "echo marker > \"$TMPDIR/probe\"".into()],
                None,
                None,
            )
            .expect("sh runs");
        assert_eq!(exit.code, 0, "the write succeeded: {}", exit.stderr);
        let written = ws.root().join(".jan-klod").join("tmp").join("probe");
        assert!(
            written.exists(),
            "the temp file is in the workspace: {}",
            written.display()
        );
    }

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
