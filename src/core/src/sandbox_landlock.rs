//! Landlock: confine a command by re-executing the gateway, which applies
//! restrictions and execs the command (becoming it).
//!
//! # Why re-exec not `pre_exec`
//!
//! Landlock restricts the calling process + children, so `CommandExt::pre_exec`
//! (unsafe) between fork/exec is obvious. Workspace denies hand-written unsafe.
//! `CommandExt::exec` is safe: rewrite to `<gateway> confine --writable <dir>
//! [--network] -- <command> <args…>`. Child applies ruleset, execs command.
//! Verified on Linux (#48): restriction survives exec.
//!
//! `pre_exec` needs async-signal-safe work; building a ruleset allocates.
//!
//! # No quoting problem (unlike Seatbelt)
//!
//! Seatbelt embeds paths in policy language, refusing literals with quotes
//! (would inject s-expressions). Landlock takes argv entries—no syntax—so any
//! path (including quotes) reaches intact. Interface difference, not policy.
//!
//! # Platform independence
//!
//! This module rewrites commands (platform-independent, tested everywhere).
//! The gateway's `confine` subcommand (Linux-only, has `landlock` dependency)
//! applies the ruleset.

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

// ─── The Linux half: applying the ruleset ────────────────────────────────────
//
// Everything above rewrites a command and is platform-independent. What follows
// talks to the kernel, so it exists only where that kernel does.

/// Whether this kernel can enforce anything, and what to say when it cannot.
///
/// Checked at boot rather than at the first command: a kernel without Landlock
/// is a fact the runtime can report while it is still reporting other facts,
/// which is what [`crate::sandbox::host_backend`] returning `Option` is for.
///
/// Asks for the *first* Landlock version's filesystem rights as a
/// [`CompatLevel::HardRequirement`], which is an error on a kernel that has no
/// Landlock rather than a silently empty ruleset. The ruleset it builds is
/// dropped unused — this only wants the answer.
///
/// # Errors
/// The reason, ready to print, when Landlock is unavailable.
#[cfg(target_os = "linux")]
pub fn available() -> Result<(), String> {
    use landlock::{Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, ABI};

    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(Ruleset::create)
        .map(|_created| ())
        .map_err(|err| {
            format!(
                "this kernel cannot enforce Landlock ({err}); it needs 5.13 or newer, built \
                 with CONFIG_SECURITY_LANDLOCK"
            )
        })
}

/// Landlock is Linux's, so everywhere else the answer is a flat no.
///
/// Present on every platform rather than gated away, so the module — and the
/// tests that use it — type-check everywhere even though they only *run* on
/// Linux. A predicate that exists only where it succeeds cannot be compiled
/// against by the code that has to handle it failing.
///
/// # Errors
/// Always, naming the platform.
#[cfg(not(target_os = "linux"))]
pub fn available() -> Result<(), String> {
    Err(format!(
        "Landlock is a Linux mechanism and this build is for {}",
        std::env::consts::OS
    ))
}

/// The newest Landlock version whose filesystem rights this build asks for.
///
/// A **maintenance point**, and the crate's own documentation says so: "the
/// Landlock ABI should be incremented (and tested) regularly". Asking for more
/// rights *restricts* more kinds of operation, so a version left behind is a
/// hole — `Truncate` arrived in ABI 3, and a build handling only ABI 1 would let
/// a command truncate a file it cannot write. Requested best-effort, so a kernel
/// that does not know these rights is not refused for it.
#[cfg(target_os = "linux")]
const NEWEST_FS: landlock::ABI = landlock::ABI::V6;

/// The Landlock version that introduced network restriction.
#[cfg(target_os = "linux")]
const NETWORK_ABI: landlock::ABI = landlock::ABI::V4;

/// Restrict *this* process to `plan`, so the command it is about to become
/// inherits the restriction.
///
/// # What is granted
///
/// Read on `/`, and read-write beneath each `writable` path. Reads are not
/// narrowed for the same reason Seatbelt does not narrow them: the gap being
/// closed is over effects, and a command that cannot read its own toolchain does
/// not run at all.
///
/// # Two compatibility levels, on purpose
///
/// ABI 1's filesystem rights are a [`CompatLevel::HardRequirement`] — without
/// them there is no confinement to speak of, so a kernel that cannot provide
/// them must fail rather than proceed. Everything newer is best-effort, so a
/// kernel that lacks it is not refused. Denying the network is a hard
/// requirement *again*, because it needs ABI 4 and a kernel without it must not
/// be reported as having denied something it cannot.
///
/// # Errors
/// The reason, ready to print, when the ruleset cannot be built or cannot be
/// **fully** enforced. Anything short of full enforcement is a refusal:
/// [`RulesetStatus::NotEnforced`] is what a kernel without Landlock produces and
/// `PartiallyEnforced` is what a partial one does, and a partly applied sandbox
/// reported as success is indistinguishable from the real thing — which the
/// [`crate::sandbox::SandboxBackend`] contract calls the one outcome worse than
/// none.
///
/// [`RulesetStatus::NotEnforced`]: landlock::RulesetStatus::NotEnforced
#[cfg(target_os = "linux")]
pub fn apply(plan: &Confinement) -> Result<(), String> {
    use landlock::{
        Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
        RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI,
    };

    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1))
        .map_err(|err| {
            format!(
                "this kernel cannot enforce Landlock's filesystem rights ({err}); it needs \
                 5.13 or newer, built with CONFIG_SECURITY_LANDLOCK"
            )
        })?
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(NEWEST_FS))
        .map_err(|err| format!("Landlock refused the newer filesystem rights: {err}"))?;

    if !plan.network {
        ruleset = ruleset
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(AccessNet::from_all(NETWORK_ABI))
            .map_err(|err| {
                format!(
                    "`execution.sandbox.network: false` needs Landlock ABI 4 (kernel 6.7 or \
                     newer) and this kernel cannot deny a command the network ({err}). Set \
                     `network: true` to allow it deliberately, or `mode: approval-only` to \
                     stop claiming a sandbox"
                )
            })?;
    }

    let mut created = ruleset
        .set_compatibility(CompatLevel::BestEffort)
        .create()
        .map_err(|err| format!("Landlock ruleset could not be created: {err}"))?
        .add_rule(PathBeneath::new(
            PathFd::new("/").map_err(|err| format!("cannot open `/` to grant reads: {err}"))?,
            AccessFs::from_read(NEWEST_FS),
        ))
        .map_err(|err| format!("Landlock refused the read rule for `/`: {err}"))?;
    for path in &plan.writable {
        let fd = PathFd::new(path)
            .map_err(|err| format!("cannot open {} to grant writes: {err}", path.display()))?;
        created = created
            .add_rule(PathBeneath::new(fd, AccessFs::from_all(NEWEST_FS)))
            .map_err(|err| {
                format!(
                    "Landlock refused the write rule for {}: {err}",
                    path.display()
                )
            })?;
    }

    let status = created
        .restrict_self()
        .map_err(|err| format!("Landlock could not restrict this process: {err}"))?;
    match status.ruleset {
        RulesetStatus::FullyEnforced => Ok(()),
        other => Err(format!(
            "Landlock reported {other:?} rather than full enforcement, so the command is \
             refused: a partly confined command is indistinguishable from a confined one"
        )),
    }
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
