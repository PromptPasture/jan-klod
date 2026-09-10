//! Landlock: confine a command by re-executing the gateway, which restricts
//! itself and then becomes the command.
//!
//! # Why a re-exec rather than `pre_exec`
//!
//! Landlock restricts *the calling process* and its children, so the obvious
//! shape is to apply it between `fork` and `exec` — `CommandExt::pre_exec`,
//! which is an `unsafe fn`. This workspace sets `unsafe_code = "deny"` with the
//! note "hand-written unsafe is forbidden", and there is no hand-written
//! `unsafe` anywhere else in the core; the sandbox is the last place to
//! introduce the first. `CommandExt::exec` is **safe**, so the same confinement
//! is available without it: the command is rewritten as
//!
//! ```text
//! <gateway> confine --writable <dir> [--network] -- <command> <args…>
//! ```
//!
//! and that child applies the ruleset to itself and `exec`s the command,
//! *becoming* it. Verified on Linux before this was written (#48): the
//! restriction survives the `exec`, which is the premise the whole design rests
//! on.
//!
//! `pre_exec` would also have needed an argument, not just an `#[allow]`: only
//! async-signal-safe work belongs between `fork` and `exec`, and building a
//! ruleset allocates.
//!
//! # No quoting problem here, unlike Seatbelt
//!
//! [`crate::sandbox_seatbelt`] embeds paths in a *policy language* and therefore
//! has to refuse a path it cannot write literally, because a `"` would inject
//! s-expressions. Landlock takes paths as argv entries, which carry no syntax at
//! all — so any path the operator can name reaches the ruleset intact, including
//! one with a quote in it. Two backends, and the difference is the interface's,
//! not the policy's.
//!
//! # What is where
//!
//! This module is platform-independent on purpose: it rewrites a command and
//! nothing more, so it compiles and is tested on every platform. Applying the
//! ruleset is the gateway's `confine` subcommand, which is Linux-only and is
//! where the `landlock` dependency lives.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sandbox::{SandboxBackend, SandboxError, SandboxPolicy};

/// The gateway subcommand that restricts itself and execs.
///
/// Deliberately undocumented: `docs_match_config::every_documented_command_exists`
/// checks that documented commands exist, not that existing ones are documented,
/// so an internal one is allowed — and it is one an operator has no reason to
/// run.
pub const CONFINE_SUBCOMMAND: &str = "confine";

/// Names the writable paths the wrapper should grant.
pub const WRITABLE_FLAG: &str = "--writable";

/// Present when the policy permits the network.
pub const NETWORK_FLAG: &str = "--network";

/// Confines a command by re-executing `wrapper`, which restricts itself.
#[derive(Debug, Clone)]
pub struct LandlockBackend {
    /// The gateway to re-execute.
    ///
    /// Resolved once, at boot, rather than per command: on Linux
    /// `current_exe()` reads `/proc/self/exe`, which reports a *deleted* binary
    /// with a `(deleted)` suffix — so a gateway replaced or upgraded while it
    /// runs would otherwise hand every command a path that cannot be spawned.
    /// Boot is where that becomes a refusal with a reason.
    ///
    /// It is a field rather than a call so a test can point it at the binary it
    /// just built: under `cargo test`, `current_exe()` is the *test* binary,
    /// which has no `confine` subcommand, and every command would fail in a way
    /// that reads exactly like Landlock refusing.
    wrapper: PathBuf,
}

impl LandlockBackend {
    /// A backend that re-executes `wrapper`.
    #[must_use]
    pub const fn new(wrapper: PathBuf) -> Self {
        Self { wrapper }
    }

    /// A backend that re-executes this process's own binary, if it can be
    /// resolved to something that exists.
    ///
    /// # Errors
    /// [`SandboxError::Refused`] when the executable cannot be resolved — see
    /// [`Self::wrapper`] for why that is a boot-time refusal rather than a
    /// per-command surprise.
    pub fn here() -> Result<Self, SandboxError> {
        let exe = std::env::current_exe().map_err(|err| {
            SandboxError::Refused(format!(
                "Landlock confines a command by re-executing this binary, and it cannot be \
                 located ({err})"
            ))
        })?;
        if !exe.exists() {
            return Err(SandboxError::Refused(format!(
                "Landlock confines a command by re-executing this binary, and {} is no longer \
                 there — a replaced or deleted executable cannot be re-executed",
                exe.display()
            )));
        }
        Ok(Self::new(exe))
    }
}

impl SandboxBackend for LandlockBackend {
    fn name(&self) -> &'static str {
        "Landlock"
    }

    fn confine(&self, command: Command, policy: &SandboxPolicy) -> Result<Command, SandboxError> {
        let mut wrapped = Command::new(&self.wrapper);
        wrapped.arg(CONFINE_SUBCOMMAND);
        for path in &policy.writable {
            wrapped.arg(WRITABLE_FLAG).arg(path);
        }
        if policy.network {
            wrapped.arg(NETWORK_FLAG);
        }
        // `--` so a command whose own first argument starts with `-` is not read
        // as another flag of ours.
        wrapped
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        Ok(wrapped)
    }
}

/// What the `confine` subcommand was asked to allow.
///
/// Parsed from argv rather than from a serialized blob: argv entries carry no
/// syntax, so a path arrives exactly as the operator wrote it and there is
/// nothing to escape or to refuse.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Confinement {
    /// Paths the command may write under.
    pub writable: Vec<PathBuf>,
    /// Whether the command may reach the network.
    pub network: bool,
    /// The command and its arguments, after `--`.
    pub command: Vec<OsString>,
}

/// Why the `confine` subcommand could not read its own arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfineArgsError {
    /// A flag needing a value did not get one.
    MissingValue(&'static str),
    /// An argument before `--` that is not one of ours.
    Unknown(String),
    /// No `--`, or nothing after it.
    NoCommand,
}

impl std::fmt::Display for ConfineArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(f, "{flag} needs a path after it"),
            Self::Unknown(arg) => write!(f, "unexpected argument `{arg}` before `--`"),
            Self::NoCommand => write!(
                f,
                "usage: {CONFINE_SUBCOMMAND} [{WRITABLE_FLAG} <dir>]… [{NETWORK_FLAG}] -- \
                 <command> [args…]"
            ),
        }
    }
}

/// Read the `confine` subcommand's arguments.
///
/// # Errors
/// [`ConfineArgsError`] when a flag is malformed or no command follows `--`.
/// Refused rather than defaulted: a `confine` that cannot read its policy must
/// not run the command, because running it would run it *unconfined*.
pub fn parse_confine_args<I, S>(args: I) -> Result<Confinement, ConfineArgsError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut out = Confinement::default();
    let mut args = args.into_iter().map(Into::into);
    let mut saw_separator = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            saw_separator = true;
            break;
        }
        if arg == WRITABLE_FLAG {
            let value = args
                .next()
                .ok_or(ConfineArgsError::MissingValue(WRITABLE_FLAG))?;
            out.writable.push(PathBuf::from(value));
        } else if arg == NETWORK_FLAG {
            out.network = true;
        } else {
            return Err(ConfineArgsError::Unknown(
                arg.to_string_lossy().into_owned(),
            ));
        }
    }
    out.command = args.collect();
    // An empty program name is not a command: `Command::new("")` cannot be
    // spawned, and reporting that as a spawn failure later would read like the
    // sandbox refusing rather than like the caller passing nothing.
    let nothing_to_run = out.command.first().is_none_or(|program| program.is_empty());
    if !saw_separator || nothing_to_run {
        return Err(ConfineArgsError::NoCommand);
    }
    Ok(out)
}

/// Whether `path` is one this backend can grant.
///
/// Only that it exists: unlike Seatbelt there is no syntax to refuse, and
/// Landlock's own `PathFd` needs to open the path, so a missing one is the only
/// thing that cannot be granted.
#[must_use]
pub fn grantable(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_confine_args, ConfineArgsError, Confinement, LandlockBackend, CONFINE_SUBCOMMAND,
        NETWORK_FLAG, WRITABLE_FLAG,
    };
    use crate::sandbox::{SandboxBackend, SandboxMode, SandboxPolicy};
    use std::path::PathBuf;

    fn policy(writable: &[&str], network: bool) -> SandboxPolicy {
        SandboxPolicy {
            mode: SandboxMode::Os,
            writable: writable.iter().map(PathBuf::from).collect(),
            network,
            require: false,
        }
    }

    fn args_of(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_command_is_rewritten_as_the_wrapper_plus_the_original() {
        let backend = LandlockBackend::new(PathBuf::from("/opt/jan-klod-gateway"));
        let mut original = std::process::Command::new("/bin/sh");
        original.arg("-c").arg("echo hi");
        let wrapped = backend
            .confine(original, &policy(&["/work"], false))
            .expect("wraps");
        assert_eq!(wrapped.get_program(), "/opt/jan-klod-gateway");
        assert_eq!(
            args_of(&wrapped),
            [
                CONFINE_SUBCOMMAND,
                WRITABLE_FLAG,
                "/work",
                "--",
                "/bin/sh",
                "-c",
                "echo hi"
            ]
        );
    }

    #[test]
    fn network_reaches_the_wrapper_only_when_the_policy_grants_it() {
        let backend = LandlockBackend::new(PathBuf::from("/gw"));
        let denied = backend
            .confine(std::process::Command::new("/bin/true"), &policy(&[], false))
            .expect("wraps");
        assert!(!args_of(&denied).contains(&NETWORK_FLAG.to_owned()));
        let allowed = backend
            .confine(std::process::Command::new("/bin/true"), &policy(&[], true))
            .expect("wraps");
        assert!(args_of(&allowed).contains(&NETWORK_FLAG.to_owned()));
    }

    /// The difference from Seatbelt worth keeping a test on: a path with a quote
    /// in it survives, because argv has no syntax. `sandbox_seatbelt` has to
    /// refuse this exact path.
    #[test]
    fn a_path_seatbelt_would_refuse_passes_through_untouched() {
        let hostile = r#"/tmp/a") (allow file-write* (subpath "/"#;
        let backend = LandlockBackend::new(PathBuf::from("/gw"));
        let wrapped = backend
            .confine(
                std::process::Command::new("/bin/true"),
                &policy(&[hostile], false),
            )
            .expect("wraps");
        assert!(
            args_of(&wrapped).contains(&hostile.to_owned()),
            "the path reaches the wrapper as one argv entry: {:?}",
            args_of(&wrapped)
        );
    }

    /// The round trip that keeps the two halves in step: whatever `confine`
    /// writes, `parse_confine_args` reads back.
    #[test]
    fn what_the_backend_writes_is_what_the_wrapper_reads() {
        let backend = LandlockBackend::new(PathBuf::from("/gw"));
        let mut original = std::process::Command::new("/bin/sh");
        original.arg("-c").arg("echo hi");
        let wrapped = backend
            .confine(original, &policy(&["/work", "/other"], true))
            .expect("wraps");
        // Skip the subcommand itself, as `main` does when dispatching.
        let argv: Vec<String> = args_of(&wrapped).into_iter().skip(1).collect();
        assert_eq!(
            parse_confine_args(argv).expect("parses"),
            Confinement {
                writable: vec![PathBuf::from("/work"), PathBuf::from("/other")],
                network: true,
                command: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            }
        );
    }

    #[test]
    fn a_confine_that_cannot_read_its_policy_refuses_rather_than_running() {
        // Every one of these would otherwise run the command *unconfined*.
        for (argv, expected) in [
            (
                vec![WRITABLE_FLAG],
                ConfineArgsError::MissingValue(WRITABLE_FLAG),
            ),
            (
                vec!["--muddle", "--", "/bin/true"],
                ConfineArgsError::Unknown("--muddle".to_owned()),
            ),
            (vec!["--", ""], ConfineArgsError::NoCommand),
            (
                vec!["/bin/true"],
                ConfineArgsError::Unknown("/bin/true".to_owned()),
            ),
            (vec![WRITABLE_FLAG, "/work"], ConfineArgsError::NoCommand),
        ] {
            let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
            match parse_confine_args(argv.clone()) {
                Err(err) if err == expected => {}
                other => panic!("{argv:?} should be {expected:?}, got {other:?}"),
            }
        }
    }
}
