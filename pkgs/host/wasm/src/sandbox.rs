//! Subprocess effect policy (`execution.sandbox`) and the backend trait; the policy itself enforces nothing.

use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::host_fs::{FsError, Workspace};

const DEFAULT_WRITABLE: &str = ".";

/// How a command's effects are confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxMode {
    /// OS confines the command (default).
    #[default]
    Os,
    /// Nothing confines. Permission gate is the only barrier.
    ApprovalOnly,
}

/// The operator's request, not the effective policy (see [`EffectiveSandbox`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Requested mode, not effective.
    pub mode: SandboxMode,
    /// Paths command may write (resolved through workspace jail).
    pub writable: Vec<PathBuf>,
    /// Network access (denied by default; sockets are exfiltration).
    pub network: bool,
    /// Whether `os` mode is required or degrades to approval-only.
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
    /// Parse `execution.sandbox`, resolving `writable` against the workspace; strict on `mode` and `writable`, lenient on booleans.
    ///
    /// # Errors
    /// [`SandboxConfigError`] for an unknown `mode`, non-string `writable`, or entries escaping the workspace.
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

/// An OS mechanism that confines a command's effects; keeps no per-command state.
pub trait SandboxBackend: Send + Sync {
    /// The mechanism's name, for boot output — "Seatbelt", "Landlock".
    fn name(&self) -> &'static str;

    /// Return `command` confined to `policy`, ready to spawn; by value because Seatbelt must replace the program.
    ///
    /// # Errors
    /// [`SandboxError`] when unavailable or the policy is rejected; never a weaker policy or an unchanged command.
    fn confine(&self, command: Command, policy: &SandboxPolicy) -> Result<Command, SandboxError>;
}

/// A backend that confines nothing; every `confine` is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoBackend;

impl SandboxBackend for NoBackend {
    fn name(&self) -> &'static str {
        "none"
    }

    fn confine(&self, _command: Command, _policy: &SandboxPolicy) -> Result<Command, SandboxError> {
        Err(SandboxError::Unsupported)
    }
}

/// The backend this build has for the host OS; `None` includes a missing `sandbox-exec` on macOS.
#[must_use]
#[cfg(target_os = "macos")]
pub fn host_backend() -> Option<Box<dyn SandboxBackend>> {
    backend_at(std::path::Path::new(crate::sandbox_seatbelt::SANDBOX_EXEC))
}

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

/// One function so [`host_backend`] and [`absence`] cannot disagree.
#[cfg(target_os = "linux")]
fn linux_backend() -> Result<Box<dyn SandboxBackend>, String> {
    crate::sandbox_landlock::available()?;
    let backend = crate::sandbox_landlock::LandlockBackend::here().map_err(|err| match err {
        SandboxError::Refused(reason) => reason,
        SandboxError::Unsupported => "no Landlock backend in this build".to_owned(),
    })?;
    Ok(Box::new(backend))
}

/// No backend: Windows is a spike, not an implementation.
#[must_use]
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn host_backend() -> Option<Box<dyn SandboxBackend>> {
    None
}

/// [`host_backend`] with the re-executed binary named; only Landlock uses `wrapper`.
#[must_use]
#[cfg(target_os = "linux")]
pub fn host_backend_at(wrapper: &std::path::Path) -> Option<Box<dyn SandboxBackend>> {
    crate::sandbox_landlock::available().ok()?;
    wrapper.exists().then(|| {
        Box::new(crate::sandbox_landlock::LandlockBackend::new(
            wrapper.to_path_buf(),
        )) as Box<dyn SandboxBackend>
    })
}

/// [`host_backend_at`] where nothing is re-executed.
#[must_use]
#[cfg(not(target_os = "linux"))]
pub fn host_backend_at(_wrapper: &std::path::Path) -> Option<Box<dyn SandboxBackend>> {
    host_backend()
}

/// Derived beside [`host_backend`] so they cannot contradict.
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

/// The mode actually in force, and why it differs from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSandbox {
    /// The mode that will apply to commands.
    pub mode: SandboxMode,
    /// Why [`Self::mode`] differs from the request; a downgrade always carries its reason.
    pub downgrade: Option<String>,
}

impl SandboxPolicy {
    /// What this policy amounts to given the backend; asking for approval-only is not a downgrade.
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

    /// Whether any command may run; `require: true` refuses rather than warns when nothing confines.
    #[must_use]
    pub fn permits_execution(&self, effective: &EffectiveSandbox) -> bool {
        !(self.require && effective.mode != SandboxMode::Os)
    }

    /// Why execution is refused, or `None`; not built from `downgrade`, whose wording claims a prompt confines the command.
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

    #[test]
    fn a_malformed_flag_can_only_tighten() {
        let policy = parse(r#"{ "sandbox": { "network": "yes", "require": 1 } }"#).unwrap();
        assert!(!policy.network, "`\"yes\"` must not read as a grant");
        assert!(!policy.require);
    }

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
        let Err(err) = NoBackend.confine(Command::new("true"), &policy) else {
            panic!("a backend that confines nothing must refuse, not hand the command back")
        };
        assert_eq!(err, SandboxError::Unsupported);
        assert_eq!(NoBackend.name(), "none");
    }

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

    #[test]
    fn without_require_an_unbackable_request_still_runs() {
        let policy = parse(r#"{ "sandbox": { "mode": "os" } }"#).unwrap();
        let effective = policy.resolve(None);
        assert_eq!(effective.mode, SandboxMode::ApprovalOnly);
        assert!(policy.permits_execution(&effective));
    }

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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_has_the_seatbelt_backend() {
        let backend = host_backend()
            .expect("/usr/bin/sandbox-exec ships with macOS; see the sibling test for its absence");
        assert_eq!(backend.name(), "Seatbelt");
    }

    /// `if let ... else`, not `match`: `clippy::single_match_else` fires on Linux-only code macOS clippy never compiles.
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
