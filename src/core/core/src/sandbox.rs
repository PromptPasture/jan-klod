//! What a subprocess may do — the policy, and nothing that enforces it.
//!
//! [`crate::host_process`] confines the *caller*: default-deny, a cwd jailed to
//! the workspace, a timeout, an output cap, a rebuilt environment. It does not
//! confine the *command*, which runs with the user's privileges and can read or
//! write anywhere the user can — the gap `docs/concepts/security-model.md`
//! records under "`host-process` confines the caller, not the command".
//!
//! Closing it needs an OS backend (Seatbelt, Landlock), and those come later.
//! What comes first is the thing such a backend would enforce, and the thing
//! the runtime needs in order to be honest before one exists: a named grant in
//! `config.yaml`, and a mode that says plainly when nothing is enforcing it.
//!
//! Nothing in this module confines anything. [`SandboxPolicy`] is a value.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::host_fs::{FsError, Workspace};

/// The `writable` entry assumed when the operator names none: the workspace
/// root, which is already the only place `host-fs` and the exec cwd allow.
const DEFAULT_WRITABLE: &str = ".";

/// How a command's effects are confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// The OS confines the command itself. The default, and what an operator
    /// gets by asking for nothing.
    #[default]
    Os,
    /// Nothing confines the command. The permission gate's confirmation is the
    /// only barrier, and the user is told so on every turn that runs one.
    ApprovalOnly,
}

/// What the operator asked for in `execution.sandbox`.
///
/// This is the *request*. Whether it can be honoured depends on the backends a
/// build has for the host OS, which is resolved at boot — a policy asking for
/// [`SandboxMode::Os`] on a platform with no backend is a legitimate config,
/// and reporting that honestly is the point of [`SandboxMode::ApprovalOnly`]
/// existing as a named state rather than as silence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// The requested mode, not the effective one.
    pub mode: SandboxMode,
    /// Absolute paths a command may write to, each already resolved through the
    /// workspace jail, so an entry here cannot name somewhere `host-fs` would
    /// refuse.
    pub writable: Vec<PathBuf>,
    /// Whether a command may reach the network. Denied by default: a sandboxed
    /// command that can still open sockets is an exfiltration path with extra
    /// steps.
    pub network: bool,
    /// Whether [`SandboxMode::Os`] is a requirement rather than a preference.
    ///
    /// False means an unbackable `os` request degrades to approval-only with a
    /// warning. True means it fails instead, for an operator who would rather
    /// have no command run than one run unconfined.
    pub require: bool,
}

/// Why an `execution.sandbox` block was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxConfigError {
    /// `mode` was neither `os` nor `approval-only`.
    UnknownMode(String),
    /// `writable` was present but was not a list of strings.
    WritableNotAList,
    /// A `writable` entry named somewhere outside the workspace.
    WritableOutsideWorkspace {
        /// The entry as the operator wrote it.
        entry: String,
        /// Why the workspace refused it.
        reason: FsError,
    },
}

impl SandboxPolicy {
    /// Read `execution.sandbox`, resolving `writable` against `workspace`.
    ///
    /// `execution` is the top-level block exactly as the config crate preserved
    /// it — opaque JSON, since the core interprets these keys and the config
    /// crate does not. Absent block, absent `sandbox`, or an empty one all give
    /// the default policy.
    ///
    /// Strict about `mode` and about `writable`, lenient about the two booleans,
    /// and the asymmetry is deliberate. A misspelt `mode` has to be an error
    /// because neither fallback is safe to guess at: defaulting to `os` claims
    /// confinement the operator may not get, and defaulting to `approval-only`
    /// silently drops confinement they asked for. A `writable` entry that
    /// escapes the workspace is a contradiction — the runtime will not honour
    /// it, so accepting it would make the list mean something other than what it
    /// says. Whereas a malformed `network` or `require` value falls back to
    /// `false`, which can only ever *tighten* the policy, so reading it
    /// leniently costs nothing.
    ///
    /// # Errors
    /// [`SandboxConfigError`] when `mode` is unrecognised, `writable` is not a
    /// list of strings, or one of its entries leaves the workspace.
    pub fn from_config(
        execution: Option<&Value>,
        workspace: &Workspace,
    ) -> Result<Self, SandboxConfigError> {
        let sandbox = execution.and_then(|block| block.get("sandbox"));
        let mode = match sandbox.and_then(|s| s.get("mode")).and_then(Value::as_str) {
            None => SandboxMode::default(),
            Some("os") => SandboxMode::Os,
            Some("approval-only") => SandboxMode::ApprovalOnly,
            Some(other) => return Err(SandboxConfigError::UnknownMode(other.to_owned())),
        };

        let requested: Vec<String> = match sandbox.and_then(|s| s.get("writable")) {
            None => vec![DEFAULT_WRITABLE.to_owned()],
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or(SandboxConfigError::WritableNotAList)
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(SandboxConfigError::WritableNotAList),
        };
        let writable = requested
            .into_iter()
            .map(|entry| {
                workspace.resolve(&entry).map_err(|reason| {
                    SandboxConfigError::WritableOutsideWorkspace { entry, reason }
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            mode,
            writable,
            network: flag(sandbox, "network"),
            require: flag(sandbox, "require"),
        })
    }
}

/// Why a backend could not confine a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// This build has no backend for the host OS.
    Unsupported,
    /// A backend exists but would not apply the policy.
    Refused(String),
}

/// An OS mechanism that confines a command's effects.
///
/// Implementations are per-platform and land in their own slices — Seatbelt on
/// macOS, Landlock on Linux. The trait is here now because the policy above is
/// meaningless without a named place for enforcement to arrive, and because the
/// boot path has to be able to say that the place is empty.
/// `Send + Sync` because the runner that holds one is shared across the
/// component host, which requires it — and because a backend has no per-command
/// state to make that awkward: it turns a policy into a confined command and
/// keeps nothing.
pub trait SandboxBackend: Send + Sync {
    /// The mechanism's name, for boot output — "Seatbelt", "Landlock".
    fn name(&self) -> &'static str;

    /// Return `command` confined to `policy`, ready to spawn.
    ///
    /// # Why this takes and returns the command rather than borrowing it
    ///
    /// Because one of the two mechanisms in the roadmap works by **replacing**
    /// the program. Seatbelt confines a child by running it under
    /// `sandbox-exec -p <profile> -- <command>`, and `std::process::Command`
    /// cannot be told to change its program: `get_program`, `get_args`,
    /// `get_envs` and `get_current_dir` read it, and nothing writes it. A
    /// `&mut Command` can therefore express an in-process mechanism (Landlock
    /// restricting the child before `exec`) and cannot express a wrapping one.
    /// Owned-in, owned-out expresses both — a wrapper builds a new command from
    /// the parts of the old one, an in-process mechanism hands back the same
    /// command with its own hook attached.
    ///
    /// This signature was `confine(&mut Command)` in 15a, where the only
    /// implementation was one that confines nothing and the difference could not
    /// show.
    ///
    /// # Errors
    /// [`SandboxError`] when the mechanism is unavailable or rejects the policy.
    /// A backend must fail rather than apply a weaker policy than asked for:
    /// partial confinement reported as success is the one outcome worse than
    /// none, because it is indistinguishable from the real thing. Returning the
    /// command unchanged is exactly that failure, so a backend that cannot
    /// confine returns `Err` instead.
    fn confine(&self, command: Command, policy: &SandboxPolicy) -> Result<Command, SandboxError>;
}

/// A backend that confines nothing.
///
/// Not a placeholder to be swapped out — it is the honest answer on any platform
/// whose backend has not been written, and it exists so tests can hold a
/// backend-shaped thing that refuses.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBackend;

impl SandboxBackend for NoBackend {
    fn name(&self) -> &'static str {
        "none"
    }

    fn confine(&self, _command: Command, _policy: &SandboxPolicy) -> Result<Command, SandboxError> {
        // The command is dropped rather than handed back: returning it would be
        // returning an unconfined command from a call whose whole purpose is to
        // confine one.
        Err(SandboxError::Unsupported)
    }
}

/// The backend this build has for the host OS, if any.
///
/// `Option` rather than always handing back a [`NoBackend`], because "there is
/// nothing here" is then a fact the boot path can state up front instead of one
/// it discovers by trying to confine a command that is about to run.
///
/// On macOS that up-front answer includes whether the *mechanism* is present:
/// Seatbelt is applied by `sandbox-exec`, and a build that has the backend on a
/// system missing the tool has nothing either. Discovering that at the first
/// command is exactly what this returning `Option` exists to avoid.
#[must_use]
#[cfg(target_os = "macos")]
pub fn host_backend() -> Option<Box<dyn SandboxBackend>> {
    backend_at(std::path::Path::new(crate::sandbox_seatbelt::SANDBOX_EXEC))
}

/// [`host_backend`] with the mechanism's path as an argument, so the "it is not
/// there" branch is reachable from a test. It cannot be reached by deleting
/// `/usr/bin/sandbox-exec`.
#[cfg(target_os = "macos")]
fn backend_at(sandbox_exec: &std::path::Path) -> Option<Box<dyn SandboxBackend>> {
    sandbox_exec
        .exists()
        .then(|| Box::new(crate::sandbox_seatbelt::SeatbeltBackend) as Box<dyn SandboxBackend>)
}

/// Landlock, when the kernel has it and this executable can be re-executed.
#[must_use]
#[cfg(target_os = "linux")]
pub fn host_backend() -> Option<Box<dyn SandboxBackend>> {
    linux_backend().ok()
}

/// The Linux backend, or the reason there is none.
///
/// One function for both answers, so [`host_backend`] and [`absence`] cannot
/// disagree about *why* — the same mistake the macOS pair avoids the same way.
/// Two things can be missing: the kernel's Landlock, and this executable (which
/// Landlock confines a command by re-executing).
#[cfg(target_os = "linux")]
fn linux_backend() -> Result<Box<dyn SandboxBackend>, String> {
    crate::sandbox_landlock::available()?;
    let backend = crate::sandbox_landlock::LandlockBackend::here().map_err(|err| match err {
        SandboxError::Refused(reason) => reason,
        SandboxError::Unsupported => "no Landlock backend in this build".to_owned(),
    })?;
    Ok(Box::new(backend))
}

/// No backend: this build has one for macOS and Linux, and Windows is a spike
/// (Slice 15d) rather than an implementation.
#[must_use]
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn host_backend() -> Option<Box<dyn SandboxBackend>> {
    None
}

/// Why there is no backend, phrased for the operator reading a boot warning.
///
/// One function rather than a message built where it is used, so it cannot
/// contradict [`host_backend`]: on macOS the *only* way to have no backend is
/// for `sandbox-exec` to be missing, so that is what this says, and elsewhere
/// the build simply has none. Both spellings name the platform, because the
/// operator's next question is "on this machine, or at all?".
fn absence() -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "this build's sandbox backend for {} is unavailable: {} is not present",
            std::env::consts::OS,
            crate::sandbox_seatbelt::SANDBOX_EXEC
        )
    }
    #[cfg(target_os = "linux")]
    {
        // Recomputed rather than remembered, and from the same function that
        // decided: either the kernel has no Landlock or this executable cannot
        // be re-executed, and the operator needs to know which.
        linux_backend().err().map_or_else(
            || {
                format!(
                    "this build's sandbox backend for {} was unavailable and now is not — \
                     nothing to report, which should not happen",
                    std::env::consts::OS
                )
            },
            |detail| {
                format!(
                    "this build's sandbox backend for {} is unavailable: {detail}",
                    std::env::consts::OS
                )
            },
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        format!(
            "this build has no sandbox backend for {}",
            std::env::consts::OS
        )
    }
}

/// The mode actually in force, and why it is not the one that was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSandbox {
    /// The mode that will apply to commands.
    pub mode: SandboxMode,
    /// Why [`Self::mode`] differs from the request — `None` when it does not.
    ///
    /// A downgrade always carries its reason, because a downgrade without one is
    /// precisely the silent weakening this module exists to prevent. Callers
    /// print it at boot and surface it on every turn that runs a command.
    pub downgrade: Option<String>,
}

impl SandboxPolicy {
    /// What this policy amounts to, given the backend available.
    ///
    /// Asking for [`SandboxMode::ApprovalOnly`] and getting it is not a
    /// downgrade, so it carries no reason. Asking for [`SandboxMode::Os`] where
    /// nothing can enforce it is, and does.
    #[must_use]
    pub fn resolve(&self, backend: Option<&dyn SandboxBackend>) -> EffectiveSandbox {
        match (self.mode, backend) {
            (SandboxMode::ApprovalOnly, _) => EffectiveSandbox {
                mode: SandboxMode::ApprovalOnly,
                downgrade: None,
            },
            (SandboxMode::Os, Some(_)) => EffectiveSandbox {
                mode: SandboxMode::Os,
                downgrade: None,
            },
            (SandboxMode::Os, None) => EffectiveSandbox {
                mode: SandboxMode::ApprovalOnly,
                downgrade: Some(format!(
                    "`execution.sandbox.mode: os` was requested, but {} — a command is \
                     confined only by the confirmation prompt",
                    absence()
                )),
            },
        }
    }

    /// Whether any command may run at all, given what [`Self::resolve`] concluded.
    ///
    /// `require: true` is an operator saying they would rather no command ran
    /// than one ran unconfined, so an approval-only outcome refuses instead of
    /// warning. Decided here, at boot, rather than per-exec: the guest-facing
    /// `proc-error` is a bare enum with no room for a reason, so a refusal
    /// discovered at exec time would reach the caller as an ordinary "denied"
    /// and tell nobody why. Boot can print the reason; `exec` cannot.
    ///
    /// `mode: approval-only` together with `require: true` is contradictory —
    /// requiring an OS sandbox while asking for none — and resolves to a
    /// refusal, which is the reading that cannot grant more than was asked for.
    #[must_use]
    pub fn permits_execution(&self, effective: &EffectiveSandbox) -> bool {
        !(self.require && effective.mode != SandboxMode::Os)
    }

    /// Why execution is refused, or `None` when it is not.
    ///
    /// Kept beside [`Self::permits_execution`] rather than composed by the
    /// caller, and deliberately *not* built from [`EffectiveSandbox::downgrade`]:
    /// that sentence ends by saying a command is confined only by the
    /// confirmation prompt, which is true of a downgrade and false of a
    /// refusal, where no command runs at all. Reusing it produced a notice that
    /// contradicted itself within one line.
    #[must_use]
    pub fn refusal(&self, effective: &EffectiveSandbox) -> Option<String> {
        if self.permits_execution(effective) {
            return None;
        }
        Some(match self.mode {
            SandboxMode::Os => format!(
                "`execution.sandbox.require: true` demands an OS sandbox and this build has \
                 none for {} — host-process is denied and no command will run. Set \
                 `require: false` to accept approval-only instead.",
                std::env::consts::OS
            ),
            SandboxMode::ApprovalOnly => "`execution.sandbox.require: true` cannot be \
                 satisfied by `mode: approval-only` — the two ask for opposite things. \
                 host-process is denied and no command will run."
                .to_owned(),
        })
    }
}

/// A boolean that defaults to `false`, so anything unreadable tightens.
fn flag(sandbox: Option<&Value>, name: &str) -> bool {
    sandbox
        .and_then(|s| s.get(name))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace in a fresh temp dir, the same shape `host_process`'s tests
    /// use — no dev-dependency for a directory.
    fn workspace() -> Workspace {
        let dir = std::env::temp_dir().join(format!(
            "jk-sandbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Workspace::open(&dir).unwrap()
    }

    fn parse(yaml_ish: &str) -> Result<SandboxPolicy, SandboxConfigError> {
        let execution: Value = serde_json::from_str(yaml_ish).unwrap();
        SandboxPolicy::from_config(Some(&execution), &workspace())
    }

    #[test]
    fn an_absent_block_is_the_default_policy() {
        let ws = workspace();
        let policy = SandboxPolicy::from_config(None, &ws).unwrap();
        assert_eq!(policy.mode, SandboxMode::Os);
        assert_eq!(policy.writable, vec![ws.root().to_path_buf()]);
        assert!(!policy.network, "the network is not granted by silence");
        assert!(!policy.require);
    }

    /// `execution:` exists today without a `sandbox:` key in it, so this is the
    /// state every currently-deployed config is in.
    #[test]
    fn an_execution_block_without_a_sandbox_key_is_the_default_policy() {
        let policy = parse(r#"{ "enabled": true, "timeout-secs": 30 }"#).unwrap();
        assert_eq!(policy.mode, SandboxMode::Os);
        assert_eq!(policy.writable.len(), 1);
    }

    #[test]
    fn approval_only_is_spelled_as_config_spells_it() {
        let policy = parse(r#"{ "sandbox": { "mode": "approval-only" } }"#).unwrap();
        assert_eq!(policy.mode, SandboxMode::ApprovalOnly);
    }

    #[test]
    fn an_unrecognised_mode_is_an_error_rather_than_either_default() {
        assert_eq!(
            parse(r#"{ "sandbox": { "mode": "sandboxed" } }"#),
            Err(SandboxConfigError::UnknownMode("sandboxed".to_owned())),
            "guessing `os` claims confinement, guessing `approval-only` drops it"
        );
    }

    #[test]
    fn writable_entries_resolve_inside_the_workspace() {
        let ws = workspace();
        std::fs::create_dir_all(ws.root().join("build")).unwrap();
        let execution: Value =
            serde_json::from_str(r#"{ "sandbox": { "writable": [".", "build"] } }"#).unwrap();
        let policy = SandboxPolicy::from_config(Some(&execution), &ws).unwrap();
        assert_eq!(
            policy.writable,
            vec![ws.root().to_path_buf(), ws.root().join("build")]
        );
    }

    #[test]
    fn a_writable_entry_that_leaves_the_workspace_is_refused() {
        let error = parse(r#"{ "sandbox": { "writable": ["../etc"] } }"#).unwrap_err();
        assert_eq!(
            error,
            SandboxConfigError::WritableOutsideWorkspace {
                entry: "../etc".to_owned(),
                reason: FsError::Denied,
            },
            "a writable root the runtime will not honour must not read as granted"
        );
    }

    #[test]
    fn an_absolute_writable_entry_is_refused_too() {
        assert!(matches!(
            parse(r#"{ "sandbox": { "writable": ["/etc"] } }"#),
            Err(SandboxConfigError::WritableOutsideWorkspace { .. })
        ));
    }

    #[test]
    fn writable_must_be_a_list_of_strings() {
        assert_eq!(
            parse(r#"{ "sandbox": { "writable": "build" } }"#),
            Err(SandboxConfigError::WritableNotAList)
        );
        assert_eq!(
            parse(r#"{ "sandbox": { "writable": ["build", 7] } }"#),
            Err(SandboxConfigError::WritableNotAList)
        );
    }

    #[test]
    fn network_and_require_are_read_when_given() {
        let policy = parse(r#"{ "sandbox": { "network": true, "require": true } }"#).unwrap();
        assert!(policy.network);
        assert!(policy.require);
    }

    /// The lenient half of the asymmetry: a value that is not a boolean falls
    /// back to `false`, which denies rather than grants.
    #[test]
    fn a_malformed_flag_can_only_tighten() {
        let policy = parse(r#"{ "sandbox": { "network": "yes", "require": 1 } }"#).unwrap();
        assert!(!policy.network, "`\"yes\"` must not read as a grant");
        assert!(!policy.require);
    }

    // ─── Effective mode ─────────────────────────────────────────────────────

    #[test]
    fn os_mode_with_no_backend_becomes_approval_only_and_says_why() {
        let policy = parse(r#"{ "sandbox": { "mode": "os" } }"#).unwrap();
        let effective = policy.resolve(None);
        assert_eq!(effective.mode, SandboxMode::ApprovalOnly);
        let reason = effective
            .downgrade
            .expect("a downgrade without a reason is the silent downgrade this prevents");
        assert!(
            reason.contains(std::env::consts::OS),
            "the reason names the platform that has no backend: {reason}"
        );
        assert!(
            reason.contains("confirmation"),
            "and says what is left protecting the user: {reason}"
        );
    }

    /// Stands in for a real backend. `resolve` decides on a backend's
    /// *presence*; whether the mechanism then works is `confine`'s answer, not
    /// `resolve`'s, so this one need not do anything.
    struct Stub;
    impl SandboxBackend for Stub {
        fn name(&self) -> &'static str {
            "stub"
        }
        fn confine(&self, command: Command, _: &SandboxPolicy) -> Result<Command, SandboxError> {
            Ok(command)
        }
    }

    #[test]
    fn os_mode_with_a_backend_stays_os_and_is_not_a_downgrade() {
        let policy = parse(r#"{ "sandbox": { "mode": "os" } }"#).unwrap();
        let effective = policy.resolve(Some(&Stub));
        assert_eq!(effective.mode, SandboxMode::Os);
        assert_eq!(effective.downgrade, None);
    }

    /// Getting what you asked for is not a downgrade, so it must not be
    /// reported as one — an operator who chose approval-only deliberately
    /// should not be warned about their own choice on every turn.
    #[test]
    fn asking_for_approval_only_is_not_a_downgrade() {
        let policy = parse(r#"{ "sandbox": { "mode": "approval-only" } }"#).unwrap();
        let effective = policy.resolve(None);
        assert_eq!(effective.mode, SandboxMode::ApprovalOnly);
        assert_eq!(effective.downgrade, None);
    }

    #[test]
    fn no_backend_refuses_rather_than_confining_nothing_quietly() {
        let policy = parse("{}").unwrap();
        // `Command` has no `Debug`-comparable equality, so the outcome is
        // matched rather than compared — and matching is what says the `Ok`
        // arm is unreachable here, which is the property under test.
        let Err(err) = NoBackend.confine(Command::new("true"), &policy) else {
            panic!("a backend that confines nothing must refuse, not hand the command back")
        };
        assert_eq!(err, SandboxError::Unsupported);
        assert_eq!(NoBackend.name(), "none");
    }

    // ─── require ────────────────────────────────────────────────────────────

    #[test]
    fn require_refuses_execution_when_nothing_can_confine() {
        let policy = parse(r#"{ "sandbox": { "mode": "os", "require": true } }"#).unwrap();
        let effective = policy.resolve(None);
        assert!(
            !policy.permits_execution(&effective),
            "an operator who required a sandbox gets no command, not an unconfined one"
        );
    }

    #[test]
    fn require_permits_execution_once_a_backend_exists() {
        let policy = parse(r#"{ "sandbox": { "mode": "os", "require": true } }"#).unwrap();
        assert!(policy.permits_execution(&policy.resolve(Some(&Stub))));
    }

    /// Without `require`, the same unbackable request degrades instead — that is
    /// the whole difference between the two settings, so it is asserted rather
    /// than left to the reader of `permits_execution`.
    #[test]
    fn without_require_an_unbackable_request_still_runs() {
        let policy = parse(r#"{ "sandbox": { "mode": "os" } }"#).unwrap();
        let effective = policy.resolve(None);
        assert_eq!(effective.mode, SandboxMode::ApprovalOnly);
        assert!(policy.permits_execution(&effective));
    }

    /// A contradictory pair: requiring an OS sandbox while asking for none.
    /// Refusing is the reading that cannot grant more than was written.
    #[test]
    fn requiring_os_while_asking_for_approval_only_refuses() {
        let policy =
            parse(r#"{ "sandbox": { "mode": "approval-only", "require": true } }"#).unwrap();
        let effective = policy.resolve(None);
        assert_eq!(effective.downgrade, None, "it is not a downgrade");
        assert!(
            !policy.permits_execution(&effective),
            "but it is still a refusal"
        );
    }

    /// The refusal notice must not borrow the downgrade's wording, which ends by
    /// saying a command is confined only by the confirmation prompt. Under a
    /// refusal there is no command, so that sentence would be false — and it
    /// was, in the first version of this.
    #[test]
    fn a_refusal_does_not_claim_a_command_still_runs() {
        for config in [
            r#"{ "sandbox": { "mode": "os", "require": true } }"#,
            r#"{ "sandbox": { "mode": "approval-only", "require": true } }"#,
        ] {
            let policy = parse(config).unwrap();
            let refusal = policy
                .refusal(&policy.resolve(None))
                .expect("a refusal has a reason");
            assert!(
                refusal.contains("no command will run"),
                "says nothing runs: {refusal}"
            );
            assert!(
                !refusal.contains("confirmation prompt"),
                "and does not describe what protects a command that will not run: {refusal}"
            );
        }
    }

    #[test]
    fn a_permitted_policy_has_no_refusal() {
        let policy = parse(r#"{ "sandbox": { "mode": "os" } }"#).unwrap();
        assert_eq!(policy.refusal(&policy.resolve(None)), None);
    }

    /// This is the state of every platform today, and the test says so out loud
    /// so that the first slice to add a backend has to come here and change it.
    /// 15b landed the macOS backend, so the old "no platform has one yet" is
    /// retired in favour of one assertion per platform — each true where it
    /// runs, rather than one that is vacuous on both.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_has_the_seatbelt_backend() {
        let backend = host_backend()
            .expect("/usr/bin/sandbox-exec ships with macOS; see the sibling test for its absence");
        assert_eq!(backend.name(), "Seatbelt");
    }

    /// Linux has one too since 15c, and it can be absent for two different
    /// reasons — so what is asserted is that the platform gets an answer, and
    /// that an absent one still names the platform.
    ///
    /// `if let … else` rather than a two-arm `match`, which trips
    /// `clippy::single_match_else`. Noted because the mistake is invisible from
    /// where this repository is developed: macOS `cargo clippy` never compiles
    /// a `#[cfg(target_os = "linux")]` function, so the `match` form linted
    /// clean locally and turned CI red. Reproducing it meant pointing this
    /// `cfg` at macOS for one run, which is the cheap way to lint per-OS code
    /// here: no Linux target is installed, and adding one to lint from macOS
    /// would build this workspace's dependency graph a second time.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_either_has_landlock_or_says_what_is_missing() {
        if let Some(backend) = host_backend() {
            assert_eq!(backend.name(), "Landlock");
        } else {
            let reason = super::absence();
            assert!(reason.contains(std::env::consts::OS), "{reason}");
            assert!(
                reason.contains("Landlock") || reason.contains("re-execut"),
                "the reason says which of the two things is missing: {reason}"
            );
        }
    }

    /// Everywhere else there is still nothing, and the reason still names the
    /// platform — which is what an operator reading the boot warning needs.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn a_platform_with_no_backend_says_which_platform() {
        assert!(
            host_backend().is_none(),
            "this build has backends for macOS and Linux only"
        );
        let reason = super::absence();
        assert!(reason.contains(std::env::consts::OS), "{reason}");
    }

    /// The branch that cannot be reached by deleting `/usr/bin/sandbox-exec`:
    /// macOS with the mechanism missing has no backend, and the reason says so
    /// rather than claiming the build has none.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_without_sandbox_exec_has_no_backend_and_blames_the_tool() {
        assert!(
            super::backend_at(std::path::Path::new("/nonexistent/sandbox-exec")).is_none(),
            "no mechanism, no backend"
        );
        let reason = super::absence();
        assert!(
            reason.contains("/usr/bin/sandbox-exec"),
            "the reason names the missing tool rather than blaming the build: {reason}"
        );
        assert!(reason.contains(std::env::consts::OS), "{reason}");
    }
}
