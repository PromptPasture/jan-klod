//! Landlock backend: confine a command by re-executing the gateway (`confine` subcommand), which
//!
//! restricts itself and execs the command. Re-exec rather than `pre_exec` because the workspace
//! denies unsafe and building a ruleset allocates (not async-signal-safe). Argv carries paths
//! with no syntax, so unlike Seatbelt no quoting is needed. The `confine` half is Linux-only.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sandbox::{SandboxBackend, SandboxError, SandboxPolicy};

/// The gateway subcommand that restricts itself and execs.
///
/// Deliberately undocumented: `docs_match_config::every_documented_command_exists` allows it
/// and operators have no reason to run it.
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
    /// Resolved once at boot: `current_exe()` reports a replaced binary with a `(deleted)` suffix,
    /// which would otherwise break every command. A field so tests can point it at the real
    /// gateway (under `cargo test` `current_exe()` has no `confine` subcommand).
    wrapper: PathBuf,
}

impl LandlockBackend {
    /// A backend that re-executes `wrapper`.
    #[must_use]
    pub const fn new(wrapper: PathBuf) -> Self {
        Self { wrapper }
    }

    /// A backend re-executing this process's own binary, if it resolves to something that exists.
    ///
    /// # Errors
    /// [`SandboxError::Refused`] when the executable cannot be resolved (see [`Self::wrapper`]).
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
        // `--` so a command starting with `-` is not read as our flag.
        wrapped
            .arg("--")
            .arg(command.get_program())
            .args(command.get_args());
        Ok(wrapped)
    }
}

/// What the `confine` subcommand was asked to allow, parsed from argv (no syntax to escape).
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
/// [`ConfineArgsError`] when a flag is malformed or no command follows `--`; never defaults,
/// since running the command would run it unconfined.
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
    // An empty program name is not a command; a later spawn failure would read like a sandbox refusal.
    let nothing_to_run = out.command.first().is_none_or(|program| program.is_empty());
    if !saw_separator || nothing_to_run {
        return Err(ConfineArgsError::NoCommand);
    }
    Ok(out)
}

/// Whether `path` can be granted: it must exist (Landlock's `PathFd` opens it); there is no syntax to refuse.
#[must_use]
pub fn grantable(path: &Path) -> bool {
    path.exists()
}

// ─── Linux half: applying the ruleset ───

/// Whether this kernel can enforce anything, and what to say when it cannot.
///
/// Checked at boot. Requests the first ABI's filesystem rights as a hard requirement so a kernel
/// without Landlock errors instead of yielding an empty ruleset; the ruleset is dropped unused.
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

/// Landlock is Linux's, so elsewhere the answer is a flat no.
///
/// Present on every platform so the module and its tests type-check everywhere.
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

/// The newest Landlock ABI whose filesystem rights this build asks for.
///
/// Maintenance point: a version left behind is a hole (`Truncate` arrived in ABI 3). Best-effort,
/// so older kernels are not refused.
#[cfg(target_os = "linux")]
const NEWEST_FS: landlock::ABI = landlock::ABI::V6;

/// The Landlock version that introduced network restriction.
#[cfg(target_os = "linux")]
const NETWORK_ABI: landlock::ABI = landlock::ABI::V4;

/// Restrict *this* process to `plan` so the command it becomes inherits the restriction.
///
/// Grants read on `/` (a command that cannot read its toolchain does not run, as with Seatbelt)
/// and read-write beneath each `writable` path. ABI 1 filesystem rights and network denial (ABI 4)
/// are hard requirements; everything newer is best-effort.
///
/// # Errors
/// The reason, ready to print, when the ruleset cannot be built or **fully** enforced:
/// partial enforcement reported as success breaks the [`crate::sandbox::SandboxBackend`] contract.
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

    // `/dev/null` for every command (#212); see `DEV_NULL_RULE` in `sandbox_seatbelt.rs`.
    // `WriteFile` alone, not `from_all`: directory rights are invalid on a file rule and Landlock
    // refuses the whole ruleset.
    created = created
        .add_rule(PathBeneath::new(
            PathFd::new("/dev/null")
                .map_err(|err| format!("cannot open `/dev/null` to grant writes: {err}"))?,
            AccessFs::WriteFile,
        ))
        .map_err(|err| format!("Landlock refused the write rule for `/dev/null`: {err}"))?;
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

    /// A path with a quote survives, because argv has no syntax (Seatbelt refuses it).
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

    /// `confine` output round-trips through `parse_confine_args`.
    #[test]
    fn what_the_backend_writes_is_what_the_wrapper_reads() {
        let backend = LandlockBackend::new(PathBuf::from("/gw"));
        let mut original = std::process::Command::new("/bin/sh");
        original.arg("-c").arg("echo hi");
        let wrapped = backend
            .confine(original, &policy(&["/work", "/other"], true))
            .expect("wraps");
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
        // Each would otherwise run the command unconfined.
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
