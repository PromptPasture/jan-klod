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
}
