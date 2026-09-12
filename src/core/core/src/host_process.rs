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
use crate::sandbox::{SandboxBackend, SandboxPolicy};

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
    /// What confines the command itself, when anything does.
    ///
    /// `None` is approval-only: the bounds above still apply — they bound the
    /// *caller* — and the command runs with the user's privileges. The boot path
    /// only fills this in when it has already reported the mode as `Os`, so that
    /// what the runtime says about confinement and what it does cannot diverge.
    confinement: Option<Confinement>,
    /// The long-lived children an operator named in `execution.long-lived`.
    ///
    /// Empty by default, and empty means none — a guest with the capability and
    /// no grant can start nothing (#109).
    long_lived: Vec<LongLived>,
}

/// A long-lived child an operator named in `execution.long-lived`.
///
/// **The grant names processes; it does not permit spawning.** A guest asks for
/// a child by `name` and the host supplies the `command` and `args` from here,
/// so the widest thing a granted guest can do is start something the operator
/// wrote down. That is why this is narrower than `execution.enabled`, which
/// lets a guest choose the command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongLived {
    /// What a guest asks for.
    pub name: String,
    /// The program to run. Never supplied by the guest.
    pub command: String,
    /// Its arguments. Also never supplied by the guest.
    pub args: Vec<String>,
}

/// A backend and the policy to hand it, paired because neither confines
/// anything alone.
///
/// `Arc` rather than `Box` because [`ProcessRunner`] is `Clone` and a backend is
/// stateless — there is nothing to copy per clone.
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

    /// A runner rooted at `workspace`, with a per-command `timeout` and `output_cap`
    /// (bytes) applied to each captured stream.
    #[must_use]
    pub const fn new(workspace: Workspace, timeout: Duration, output_cap: usize) -> Self {
        Self {
            workspace: Some(workspace),
            timeout,
            output_cap,
            env_passthrough: Vec::new(),
            // Nothing confines a command until the boot path says so, and it
            // says so only when it has reported the mode as `Os`.
            confinement: None,
            // Naming a command to run is a separate grant from being allowed to
            // run commands, so `execution.enabled` alone grants none.
            long_lived: Vec::new(),
        }
    }

    /// Confine every command with `backend` under `policy`.
    ///
    /// Called by the boot path only when the effective mode is
    /// [`SandboxMode::Os`](crate::sandbox::SandboxMode::Os), which is also what
    /// it printed — the two are set from the same decision so the boot line
    /// cannot claim confinement that is not wired up.
    #[must_use]
    pub fn with_sandbox(
        mut self,
        backend: std::sync::Arc<dyn SandboxBackend>,
        policy: SandboxPolicy,
    ) -> Self {
        self.confinement = Some(Confinement { backend, policy });
        self
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

    /// Permit these, and only these, long-lived children.
    #[must_use]
    pub fn with_long_lived(mut self, children: Vec<LongLived>) -> Self {
        self.long_lived = children;
        self
    }

    /// The grant for `name`, or `None` if the operator did not name it.
    ///
    /// The whole of the admission decision: a guest supplies a name and gets
    /// back what to run, or gets back nothing. There is no path by which a
    /// guest's own string becomes a program.
    #[must_use]
    pub fn long_lived_grant(&self, name: &str) -> Option<&LongLived> {
        self.long_lived.iter().find(|child| child.name == name)
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

    /// A `Command` confined and configured, ready to spawn.
    ///
    /// Shared by [`Self::exec`] and [`Self::spawn_long_lived`] so confinement is
    /// **reused rather than restated**: a long-lived child that outlives its
    /// call is more exposed than a one-shot command, not less, so the one place
    /// that decides how a command is bounded has to be the one place both go
    /// through. Everything here is identical for both; they differ only in what
    /// they do with the `Child`.
    ///
    /// Confined *before* being configured: a backend can only carry the program
    /// and its arguments across (there is no getter for stdio, and `get_envs`
    /// cannot say whether `env_clear` was called), so cwd, the scrubbed
    /// environment and the pipes are applied to whatever it hands back — which
    /// for a wrapping mechanism is a different process.
    ///
    /// # Errors
    /// [`ProcError::Denied`] if the backend refuses to confine the command.
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
                    // The guest-facing `proc-error` is a bare enum with no room
                    // for a reason, so the reason goes to the host's log and the
                    // guest gets a denial. Denied rather than run unconfined:
                    // the operator asked for confinement and the runtime said it
                    // had it.
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

    /// Start the long-lived child granted under `name`, confined exactly as a
    /// one-shot command is.
    ///
    /// The command and its arguments come from the grant, never from the
    /// caller — see [`LongLived`].
    ///
    /// # Errors
    /// [`ProcError::Denied`] when no grant carries `name`, when there is no
    /// workspace to run in, or when the backend refuses to confine it;
    /// [`ProcError::SpawnFailed`] if it cannot be started.
    pub fn spawn_long_lived(&self, name: &str) -> Result<LiveChild, ProcError> {
        let Some(workspace) = &self.workspace else {
            return Err(ProcError::Denied);
        };
        // The admission decision, and it happens **before** anything starts. An
        // implementation that spawned and then checked would have a window in
        // which it had done neither, and a window is all a capability like this
        // needs to stop being default-deny.
        //
        // `proc-error` carries no payload, so the reason goes to the host log —
        // the same answer `exec` gives when a backend refuses to confine.
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

        let mut child = self
            .prepared(command, args, &dir)?
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

/// How long the death report waits for the stderr tail to arrive (#133).
///
/// Long enough that a thread which has already read the bytes gets scheduled,
/// short enough that nobody notices it on the path where a child was killed
/// deliberately and said nothing.
const STDERR_DRAIN: Duration = Duration::from_millis(200);

/// A long-lived child the host is holding open for a guest (#109).
///
/// # Why a reader thread rather than a poll
///
/// The core is single-threaded, and `read` on a pipe blocks until there is
/// something to read. A child that says nothing — which a stdio server does
/// whenever it has no reply yet — would therefore hang the runtime, taking the
/// turn, the transport and every other instance with it. So one thread per child
/// does the blocking read and hands whole chunks over a channel, and the guest
/// -facing read is a `recv_timeout` that always returns. The same shape
/// `core::acp`'s `drain` uses for the editor's pipe, and for the same reason.
///
/// The thread ends when the pipe closes, which is when the child exits, so
/// nothing has to stop it.
///
/// There are two such threads: one for stdout, which the guest reads, and one
/// for stderr, which only the host log ever sees (#133).
pub struct LiveChild {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    /// Chunks the reader thread has pulled off stdout.
    stdout: std::sync::mpsc::Receiver<Vec<u8>>,
    /// The tail of stderr, delivered once when that pipe reaches EOF (#133).
    ///
    /// A whole channel for one message, because the alternative — a shared
    /// buffer read at the moment the exit is noticed — loses the race it most
    /// needs to win. A child writes its complaint and *then* exits, so the bytes
    /// are still in the pipe when `try_wait` first reports the death, and a
    /// reader thread that has not been scheduled yet leaves the buffer empty at
    /// exactly the moment the buffer is worth reading. EOF is the only signal
    /// that says "this stream is finished", and the thread is the only thing
    /// that sees it.
    stderr: std::sync::mpsc::Receiver<String>,
    /// What a previous read did not take, kept so `max_bytes` bounds the
    /// *answer* rather than discarding the remainder of a chunk.
    pending: Vec<u8>,
    /// The runner's output cap, applied per read.
    cap: usize,
    /// The grant this child was started under, so a log line can name it.
    name: String,
    /// Whether the death has already been reported (#133).
    ///
    /// `is_running` is polled in a loop, and `try_wait` keeps returning the same
    /// `Ok(Some(status))` once it has reaped — so without this, "log the status
    /// when the child has exited" is a line per poll, which is a different bug
    /// from the silence it replaces.
    exit_reported: bool,
}

impl LiveChild {
    /// Take the pipes and start the reader threads.
    fn new(mut child: std::process::Child, cap: usize, name: String) -> Self {
        let stdin = child.stdin.take();
        let (tx, stdout) = std::sync::mpsc::channel();
        if let Some(mut out) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut buf = [0_u8; 8192];
                while let Ok(n) = out.read(&mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
        }
        // stderr had a thread of its own added by #133, and draining it is worth
        // as much as reading it: `prepared()` pipes stderr for every child, and
        // nothing here read a long-lived one. A pipe nobody drains fills at the
        // OS buffer — about 64 KiB — and then the child *blocks on write*. So
        // this loop keeps a chatty server running as much as it keeps its last
        // words.
        let (tx_err, stderr) = std::sync::mpsc::channel();
        if let Some(mut err) = child.stderr.take() {
            std::thread::spawn(move || {
                // The last `cap` bytes, not the first: a process explains itself
                // on the way out, so the tail is the half worth keeping. The
                // runner's own output cap bounds it, the same number that bounds
                // `exec`'s captured streams and a guest's single read — a second
                // limit invented here would be one more thing to keep in step.
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
            pending: Vec::new(),
            cap,
            name,
            exit_reported: false,
        }
    }

    /// Write to the child's stdin.
    ///
    /// # Errors
    /// [`ProcError::SpawnFailed`] if the pipe is gone or the write fails — the
    /// child is not listening, whatever the reason.
    pub fn write_stdin(&mut self, data: &str) -> Result<(), ProcError> {
        let Some(pipe) = self.stdin.as_mut() else {
            return Err(ProcError::SpawnFailed);
        };
        pipe.write_all(data.as_bytes())
            .and_then(|()| pipe.flush())
            .map_err(|_| ProcError::SpawnFailed)
    }

    /// Read at most `max_bytes`, waiting up to `timeout` for the first chunk.
    ///
    /// An empty string means nothing arrived in that window — **not** that the
    /// child is finished. [`Self::is_running`] answers that, and the two are
    /// different questions.
    pub fn read_stdout(&mut self, max_bytes: usize, timeout: Duration) -> String {
        if self.pending.is_empty() {
            if let Ok(chunk) = self.stdout.recv_timeout(timeout) {
                self.pending = chunk;
            }
        }
        // The runner's cap bounds a single read the way it bounds `exec`'s
        // captured output: a guest asking for more than the operator allows gets
        // the operator's number.
        let take = max_bytes.min(self.cap).min(self.pending.len());
        let rest = self.pending.split_off(take);
        let taken = std::mem::replace(&mut self.pending, rest);
        String::from_utf8_lossy(&taken).into_owned()
    }

    /// Whether the child is still alive.
    ///
    /// The first time it is observed to have exited, the status goes to the host
    /// log (#133). `try_wait` has always returned it and this used to discard it
    /// in the same expression that asked for it, which is why four different
    /// deaths — a wrapper rejecting its arguments, a command that does not
    /// exist, a script with a syntax error, and a clean deliberate quit — all
    /// read as `server exited` and cost #132 eight red runs to tell apart.
    ///
    /// `Err` is left as it was: it means the question could not be answered, not
    /// that the child died, and answering it wrongly in a log is worse than not
    /// answering it.
    pub fn is_running(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                if !self.exit_reported {
                    self.exit_reported = true;
                    // WARN even for a clean exit: a *long-lived* child is one
                    // something is still expecting to talk to, so it going away
                    // is unexpected whatever status it went away with.
                    eprintln!(
                        "WARN [core] host-process: long-lived child `{}` {}",
                        self.name,
                        describe_exit(status)
                    );
                    // The stderr thread sends its tail once, at EOF. Waiting
                    // briefly rather than taking whatever is there right now is
                    // the whole point of the channel: the child writes its
                    // complaint and *then* exits, so at this instant the bytes
                    // are usually still in the pipe. Bounded, because a
                    // grandchild holding the write end open means EOF never
                    // arrives, and a diagnostic must not be able to hang the
                    // runtime it is diagnosing.
                    let tail = self.stderr_tail();
                    let tail = tail.trim();
                    if !tail.is_empty() {
                        eprintln!(
                            "WARN [core] host-process: long-lived child `{}` last stderr: {tail}",
                            self.name
                        );
                    }
                }
                false
            }
            Err(_) => false,
        }
    }

    /// The last words the child wrote to stderr, once its pipe has closed.
    ///
    /// Empty when there was nothing, when the pipe is still open after
    /// [`STDERR_DRAIN`], or when the child had no stderr to begin with — a
    /// disconnected channel returns immediately, so none of those wait.
    ///
    /// Consuming: the thread sends exactly one message, so this answers once.
    /// That is all the death report needs, and it is why nothing else calls it.
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
    /// **The lifetime guarantee, and it is here rather than in a shutdown
    /// path.** A child must not outlive the instance that owns it, and the only
    /// thing guaranteed to run when that instance goes — normally, on error, or
    /// when the whole runtime drops — is this.
    fn drop(&mut self) {
        self.kill();
    }
}

/// The long-lived children one extension instance is holding open.
///
/// # Why this is a type rather than two fields on a host adapter
///
/// Both `tool-*` and `registry-*` guests can hold children, and they are
/// instantiated by different adapters (`tool_host::ToolHost`,
/// `registry_host::McpHost`). The lifetime guarantee from
/// [#109](https://github.com/PromptPasture/jan-klod/issues/109) — a child dies
/// with the instance that started it — has to hold identically for both, and a
/// guarantee implemented twice is a guarantee that will eventually be
/// implemented once. So the table, the handle counter and every guest-facing
/// operation live here, and an adapter holds one and delegates.
///
/// Dropping this kills everything in it, because dropping a [`LiveChild`] does.
#[derive(Default)]
pub struct Children {
    live: std::collections::HashMap<u32, LiveChild>,
    /// Monotonic, so a killed handle is never reissued and a stale one fails
    /// rather than addressing somebody else's child.
    next: u32,
}

impl Children {
    /// Start the child `runner` grants under `name`.
    ///
    /// # Errors
    /// Whatever [`ProcessRunner::spawn_long_lived`] refuses with — including
    /// [`ProcError::Denied`] for a name the operator never wrote down.
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
    /// [`ProcError::Denied`] for a handle this instance was not given — a guest
    /// can pass any integer, and the answer to one it never received is the
    /// answer it gets for a child it was never granted.
    pub fn write_stdin(&mut self, handle: u32, data: &str) -> Result<(), ProcError> {
        self.live
            .get_mut(&handle)
            .ok_or(ProcError::Denied)?
            .write_stdin(data)
    }

    /// Read from a child's stdout. See [`LiveChild::read_stdout`] for what an
    /// empty answer means.
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

    /// Whether a child is alive. `false` for an unknown handle, so a caller
    /// polling one it does not have terminates rather than erroring forever.
    pub fn is_running(&mut self, handle: u32) -> bool {
        self.live
            .get_mut(&handle)
            .is_some_and(LiveChild::is_running)
    }

    /// Kill and forget. Dropping the [`LiveChild`] is what kills it, so removing
    /// it from the map is the whole implementation — and the same thing happens
    /// to every child still here when this is dropped.
    pub fn kill(&mut self, handle: u32) {
        self.live.remove(&handle);
    }
}

/// Read a captured stream to a UTF-8 string, truncated to `cap` bytes.
/// How a child died, in the terms the platform actually offers.
///
/// A signal is not an exit code: on unix `status.code()` is `None` for a child
/// that was killed, so a formatter that only reads `code()` turns the most
/// interesting death — SIGSEGV, SIGKILL from an OOM killer — into "no status".
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

    /// The shared table refuses a name no grant carries — the one path both
    /// `tool_host` and `registry_host` go through (#110).
    ///
    /// Asserted here rather than across the Component-Model boundary because
    /// this is the *shared* half: a boundary test needs a guest that calls
    /// `spawn`, and `registry-mcp` does not yet. The manifest says so plainly —
    /// capabilities are read from a component's real imports, so `host-process`
    /// will not appear in `registry-mcp.manifest.toml` until the guest actually
    /// calls it. The boundary test arrives with the caller.
    #[test]
    fn a_name_no_grant_carries_is_refused_by_the_shared_table() {
        let mut children = Children::default();
        let ungranted = ProcessRunner::disabled();
        assert_eq!(
            children.spawn(&ungranted, "nothing-named-this"),
            Err(ProcError::Denied)
        );
        // And an unknown handle is refused the same way, since a guest can pass
        // any integer.
        assert_eq!(
            children.write_stdin(41, "x"),
            Err(ProcError::Denied),
            "a handle this table never issued reaches nobody"
        );
        assert!(!children.is_running(41));
    }

    /// A death has to be described in the terms the platform actually used
    /// (#133).
    ///
    /// The signal arm is the one worth asserting: `status.code()` is `None` for
    /// a child that was killed, so a describer that only reads `code()` turns
    /// the most interesting deaths — SIGSEGV, or SIGKILL from an OOM killer —
    /// into "no status", which is the same silence this issue is about.
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

    /// A child's last words survive it, and the runner's cap bounds them (#133).
    ///
    /// The size assertion is the one that matters. `prepared()` pipes stderr for
    /// every child and, before this, nothing read a long-lived one's — so a
    /// server chatty enough to fill the OS pipe buffer would have *blocked on
    /// write* with no diagnosis available anywhere. Draining it is worth as much
    /// as reading it, and an undrained pipe and an unbounded buffer are the two
    /// ways to get that wrong.
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
                // Ends with a line naming itself, so "the tail, not the head" is
                // checkable rather than merely asserted.
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
