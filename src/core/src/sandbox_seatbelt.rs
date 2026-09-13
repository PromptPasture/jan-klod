//! Seatbelt: confine a command via `sandbox-exec`.
//!
//! macOS's unprivileged sandbox (no entitlement, helper, or root). **Deprecated**
//! but present on all macOS; no unprivileged replacement. [`crate::sandbox`]
//! reports "nothing" honestly when missing.
//!
//! # Profile: deny-by-default with minimal grants
//!
//! `(deny default)`, then shell essentials: `process-exec`, `process-fork`,
//! `sysctl-read`, `file-read*`. Reads allowed wholesale (confinement targets
//! **effects**—writes change the machine, reads don't per the security model).
//!
//! Commands needing Mach services (git, cargo) fail under this profile with
//! visible errors about what was denied—the right direction to fail.

use std::path::Path;
use std::process::Command;

use crate::sandbox::{SandboxBackend, SandboxError, SandboxPolicy};

/// The profile applier. An absolute path on purpose: resolving `sandbox-exec`
/// through `PATH` would let a `PATH` entry decide what confines a command.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Confines a command with a Seatbelt profile.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeatbeltBackend;

impl SandboxBackend for SeatbeltBackend {
    fn name(&self) -> &'static str {
        "Seatbelt"
    }

    /// Rebuild `command` as `sandbox-exec -p <profile> -- <command>`.
    ///
    /// # This must be called before the command is configured
    ///
    /// Only the program and its arguments are carried over, because they are
    /// the only parts a `Command` can be read back: there is no getter for
    /// stdio, and `get_envs` reports the *changes* to the environment without
    /// saying whether `env_clear` was called. Copying what can be read would
    /// therefore silently drop the stdio wiring an output cap depends on. So the
    /// caller confines first and applies cwd, environment and stdio afterwards —
    /// those all belong to the outer `sandbox-exec` process anyway, which is the
    /// one being spawned.
    fn confine(&self, command: Command, policy: &SandboxPolicy) -> Result<Command, SandboxError> {
        let profile = profile(policy)?;
        let mut wrapped = Command::new(SANDBOX_EXEC);
        wrapped
            .arg("-p")
            .arg(profile)
            // `--` so a command whose own first argument starts with `-` is not
            // read as another sandbox-exec flag.
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        Ok(wrapped)
    }
}

/// The Seatbelt profile for `policy`.
///
/// # Errors
/// [`SandboxError::Refused`] when a `writable` path cannot be resolved or
/// cannot be written into a profile faithfully. Both refuse rather than
/// approximate: a grant this generator silently mangles is a grant the operator
/// believes they have.
pub fn profile(policy: &SandboxPolicy) -> Result<String, SandboxError> {
    let mut profile = String::from("(version 1)\n(deny default)\n");
    // The minimum a shell needs to start. See the module docs for why reads are
    // not narrowed here.
    profile.push_str("(allow process-exec)\n(allow process-fork)\n(allow sysctl-read)\n");
    profile.push_str("(allow file-read*)\n");
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

/// A `writable` path as Seatbelt will see it: symlinks resolved.
///
/// **This is the whole reason to canonicalize, and it was found by probing
/// rather than by reading.** Seatbelt matches `(subpath …)` against the
/// resolved path, and on macOS `/tmp` is a symlink to `/private/tmp` (as is
/// `/var`). A profile granting write to `/tmp/x/work` denies a write to
/// `/tmp/x/work/ok` — and denies it with "Operation not permitted", which is
/// indistinguishable from the sandbox working. Every temp directory a test
/// makes is under `/tmp`, so an uncanonicalized profile would have passed a
/// suite asserting that escapes are denied while also denying everything the
/// operator allowed.
///
/// A path that does not exist cannot be resolved, so it is refused. Writing the
/// unresolved path instead would produce exactly the silent non-grant above.
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

/// A path as an SBPL string literal, or a refusal.
///
/// SBPL is s-expressions, so a path containing `"` would close the literal
/// early and the rest of it would be **read as policy** — an injection into a
/// security decision, by way of a directory name. Escaping is not attempted:
/// this generator would be guessing at another language's escape rules, and a
/// guess that is subtly wrong here does not fail loudly, it silently widens the
/// sandbox. A path that cannot be written literally is refused instead, which
/// costs an exotic directory name the `os` mode and costs nothing else.
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

    /// A policy over a directory that exists, since the generator resolves.
    fn policy_over(dir: &std::path::Path) -> SandboxPolicy {
        SandboxPolicy {
            mode: SandboxMode::Os,
            writable: vec![dir.to_path_buf()],
            network: false,
            require: false,
        }
    }

    /// Removes the directory on drop, panic or not.
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
    fn network_is_granted_only_when_the_policy_says_so() {
        let dir = temp_dir("network");
        let mut policy = policy_over(&dir.0);
        policy.network = true;
        assert!(profile(&policy)
            .expect("generates")
            .contains("(allow network*)"));
    }

    /// The finding this module exists to remember: Seatbelt matches the resolved
    /// path, so the profile must carry the resolved path. On macOS the temp dir
    /// is under `/tmp`, a symlink to `/private/tmp`, and the unresolved form
    /// silently grants nothing.
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

    /// A grant that cannot be resolved is refused, because writing the
    /// unresolved path would grant nothing while looking granted.
    #[test]
    fn a_writable_path_that_does_not_exist_is_refused() {
        let dir = temp_dir("missing");
        let policy = policy_over(&dir.0.join("not-created-yet"));
        let Err(SandboxError::Refused(reason)) = profile(&policy) else {
            panic!("a path that cannot be resolved must be refused")
        };
        assert!(reason.contains("cannot be resolved"), "{reason}");
    }

    /// SBPL is s-expressions: a `"` in a directory name would close the literal
    /// and the remainder would be read as policy.
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

    /// The wrapper carries the program and its arguments and nothing else, per
    /// `confine`'s contract — there is no getter for stdio, so anything else
    /// would be a silent loss.
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

    /// A policy the generator refuses must refuse the *command*, not hand back
    /// an unwrapped one — the failure the trait's docs call worse than none.
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
