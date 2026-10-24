//! Seatbelt backend: confines a command via macOS `sandbox-exec`; deny-by-default, reads allowed wholesale.

use std::path::Path;
use std::process::Command;

use crate::sandbox::{SandboxBackend, SandboxError, SandboxPolicy};

/// The profile applier; absolute so a `PATH` entry cannot decide what confines a command.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Confines a command with a Seatbelt profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeatbeltBackend;

impl SandboxBackend for SeatbeltBackend {
    fn name(&self) -> &'static str {
        "Seatbelt"
    }

    /// Rebuild `command` as `sandbox-exec -p <profile> -- <command>`; apply cwd, env and stdio afterwards.
    fn confine(&self, command: Command, policy: &SandboxPolicy) -> Result<Command, SandboxError> {
        let profile = profile(policy)?;
        let mut wrapped = Command::new(SANDBOX_EXEC);
        wrapped
            .arg("-p")
            .arg(profile)
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        Ok(wrapped)
    }
}

/// Without it `2>/dev/null` makes the shell fail first; `literal`, not `subpath`, so only that device is covered.
const DEV_NULL_RULE: &str = "(allow file-write* (literal \"/dev/null\"))\n";

/// The Seatbelt profile for `policy`.
///
/// # Errors
/// [`SandboxError::Refused`] when a `writable` path cannot be resolved or faithfully written; never approximated.
pub fn profile(policy: &SandboxPolicy) -> Result<String, SandboxError> {
    let mut profile = String::from("(version 1)\n(deny default)\n");
    profile.push_str("(allow process-exec)\n(allow process-fork)\n(allow sysctl-read)\n");
    profile.push_str("(allow file-read*)\n");
    profile.push_str(DEV_NULL_RULE);
    for path in &policy.writable {
        let resolved = resolve(path)?;
        profile.push_str("(allow file-write* (subpath ");
        profile.push_str(&quote(&resolved)?);
        profile.push_str("))\n");
    }
    if policy.network {
        profile.push_str("(allow network*)\n");
    }
    Ok(profile)
}

/// Seatbelt matches `(subpath ...)` against the resolved path, and `/tmp` is a symlink into `/private`.
fn resolve(path: &Path) -> Result<String, SandboxError> {
    let resolved = path.canonicalize().map_err(|err| {
        SandboxError::Refused(format!(
            "`writable` names {} which cannot be resolved ({err}). Seatbelt matches the \
             real path, so a path that does not exist yet cannot be granted — create it, \
             or name a parent that exists.",
            path.display()
        ))
    })?;
    resolved.to_str().map(str::to_owned).ok_or_else(|| {
        SandboxError::Refused(format!(
            "`writable` names {}, whose resolved form is not UTF-8 and cannot go into a \
             profile",
            path.display()
        ))
    })
}

/// A `"` in the path would close the literal and inject policy, so such paths are refused, not escaped.
fn quote(path: &str) -> Result<String, SandboxError> {
    if let Some(bad) = path
        .chars()
        .find(|c| *c == '"' || *c == '\\' || c.is_control())
    {
        return Err(SandboxError::Refused(format!(
            "`writable` names a path containing {bad:?}, which cannot be written into a \
             Seatbelt profile without escaping rules this would be guessing at. Rename the \
             directory, or set `execution.sandbox.mode: approval-only` deliberately."
        )));
    }
    Ok(format!("\"{path}\""))
}

#[cfg(test)]
mod tests {
    use super::{profile, quote, SeatbeltBackend};
    use crate::sandbox::{SandboxBackend, SandboxError, SandboxMode, SandboxPolicy};
    use std::path::PathBuf;

    fn policy_over(dir: &std::path::Path) -> SandboxPolicy {
        SandboxPolicy {
            mode: SandboxMode::Os,
            writable: vec![dir.to_path_buf()],
            network: false,
            require: false,
        }
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn temp_dir(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("jk-seatbelt-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates a temp dir");
        TempDir(dir)
    }

    #[test]
    fn the_profile_denies_by_default_and_grants_only_what_was_asked() {
        let dir = temp_dir("default");
        let text = profile(&policy_over(&dir.0)).expect("generates");
        assert!(text.starts_with("(version 1)\n(deny default)\n"), "{text}");
        assert!(text.contains("(allow file-read*)"), "{text}");
        assert!(
            !text.contains("(allow network"),
            "network is denied unless asked for: {text}"
        );
        assert!(
            text.contains("(allow file-write* (subpath "),
            "the writable path is granted: {text}"
        );
    }

    #[test]
    fn dev_null_is_writable_without_anyone_asking() {
        let dir = temp_dir("devnull");
        let text = profile(&policy_over(&dir.0)).expect("generates");
        assert!(
            text.contains("(allow file-write* (literal \"/dev/null\"))"),
            "a shell cannot run `2>/dev/null` without this: {text}"
        );
        assert!(
            !text.contains("(subpath \"/dev\")"),
            "only the device is granted, not the directory: {text}"
        );
    }

    #[test]
    fn network_is_granted_only_when_the_policy_says_so() {
        let dir = temp_dir("network");
        let mut policy = policy_over(&dir.0);
        policy.network = true;
        assert!(profile(&policy)
            .expect("generates")
            .contains("(allow network*)"));
    }

    #[test]
    fn a_writable_path_reaches_the_profile_symlink_resolved() {
        let dir = temp_dir("resolve");
        let text = profile(&policy_over(&dir.0)).expect("generates");
        let resolved = dir.0.canonicalize().expect("the temp dir exists");
        assert!(
            text.contains(&format!("(subpath \"{}\")", resolved.display())),
            "the profile carries the resolved path, not {}: {text}",
            dir.0.display()
        );
    }

    #[test]
    fn a_writable_path_that_does_not_exist_is_refused() {
        let dir = temp_dir("missing");
        let policy = policy_over(&dir.0.join("not-created-yet"));
        let Err(SandboxError::Refused(reason)) = profile(&policy) else {
            panic!("a path that cannot be resolved must be refused")
        };
        assert!(reason.contains("cannot be resolved"), "{reason}");
    }

    #[test]
    fn a_path_that_would_inject_sbpl_is_refused_rather_than_escaped() {
        for hostile in [
            "/tmp/a\"b",
            "/tmp/a\") (allow file-write* (subpath \"/",
            "/tmp/a\\b",
            "/tmp/a\nb",
        ] {
            let Err(SandboxError::Refused(reason)) = quote(hostile) else {
                panic!("{hostile:?} must be refused, not escaped")
            };
            assert!(reason.contains("Seatbelt profile"), "{reason}");
        }
        assert_eq!(
            quote("/private/tmp/ok").expect("plain"),
            "\"/private/tmp/ok\""
        );
    }

    #[test]
    fn the_command_is_wrapped_with_the_profile_and_a_double_dash() {
        let dir = temp_dir("wrap");
        let mut original = std::process::Command::new("/bin/sh");
        original.arg("-c").arg("echo hi");
        let wrapped = SeatbeltBackend
            .confine(original, &policy_over(&dir.0))
            .expect("wraps");
        assert_eq!(wrapped.get_program(), "/usr/bin/sandbox-exec");
        let args: Vec<String> = wrapped
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "-p", "the profile is passed inline: {args:?}");
        assert!(args[1].contains("(deny default)"), "{args:?}");
        assert_eq!(
            &args[2..],
            ["--", "/bin/sh", "-c", "echo hi"],
            "the original command follows a `--`: {args:?}"
        );
    }

    #[test]
    fn a_refused_profile_refuses_the_command() {
        let dir = temp_dir("refused");
        let policy = policy_over(&dir.0.join("missing"));
        let outcome = SeatbeltBackend.confine(std::process::Command::new("/bin/sh"), &policy);
        assert!(
            matches!(outcome, Err(SandboxError::Refused(_))),
            "an unusable profile refuses rather than running the command unconfined"
        );
    }
}
