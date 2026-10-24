//! Component lifecycle: install, list, remove from `ext/`.
//! Install checks integrity, SHA-256 and minisign; `list` reads declared capabilities.

use std::path::{Path, PathBuf};

use wasmtime::component::Component;
use wasmtime::Engine;

use crate::manifest::{manifest_path, Manifest};
use crate::{inspect, Verdict};

/// The extension of a staged component.
const COMPONENT_EXT: &str = "wasm";

/// What `install` verifies beyond component well-formedness.
///
/// Invariant: a digest is required wherever the signature is waived (see [`Checks::allow_unsigned`]).
/// A digest only counts as evidence if it came by a different route than the bytes.
#[derive(Debug, Clone, Default)]
pub struct Checks {
    /// Expected SHA-256 of the component only (case-insensitive); the signature covers both files.
    pub sha256: Option<String>,

    /// Minisign public keys (base64) from `registry.trusted-keys`; empty trusts nothing.
    pub trusted_keys: Vec<String>,

    /// Waive signature requirement (requires digest; see [`Checks::sha256`]).
    pub allow_unsigned: bool,
}

impl Checks {
    /// Read `registry.trusted-keys` from the top-level `registry` block; malformed entries fail.
    ///
    /// # Errors
    /// [`ExtError::TrustedKeysNotAList`] if `trusted-keys` is not a string list.
    pub fn from_config(registry: Option<&serde_json::Value>) -> Result<Self, ExtError> {
        let keys = match registry.and_then(|block| block.get("trusted-keys")) {
            None => Vec::new(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or(ExtError::TrustedKeysNotAList)
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(ExtError::TrustedKeysNotAList),
        };
        Ok(Self {
            sha256: None,
            trusted_keys: keys,
            allow_unsigned: false,
        })
    }

    /// Read the grant from `config.yaml` (here, not gateway, to avoid a config crate dependency).
    ///
    /// # Errors
    /// [`ExtError::Config`] if file can't be read/parsed or [`Checks::from_config`] refuses.
    pub fn from_config_path(path: &Path) -> Result<Self, ExtError> {
        let registry =
            jk_config::Config::top_level(path, "registry").map_err(|err| ExtError::Config {
                path: path.display().to_string(),
                detail: err.to_string(),
            })?;
        Self::from_config(registry.as_ref())
    }
}

/// A SHA-256 digest as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Whether `install` argument is remote (http/https only; others are paths).
#[must_use]
pub fn looks_remote(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Refuse remote sources the policy would not permit, before bytes move.
///
/// Uses [`crate::egress::EgressPolicy::public_only`], not the runtime policy: origins trusted for
/// model calls are not places to fetch components from. Private sources are refused.
///
/// # Errors
/// [`ExtError::RefusedByPolicy`] naming the URL and policy rationale.
pub fn check_remote(url: &str) -> Result<(), ExtError> {
    crate::egress::EgressPolicy::public_only()
        .check(url)
        .map(|_| ())
        .map_err(|err| ExtError::RefusedByPolicy {
            url: url.to_owned(),
            detail: format!("{err:?}"),
        })
}

/// Remote install fetch list (URL names component; siblings inferred).
///
/// Convention: `https://h/p/tool-fs.wasm` → `tool-fs.manifest.toml`,
/// `tool-fs.wasm.minisig`, `tool-fs.manifest.toml.minisig` (unless unsigned).
///
/// # Errors
/// [`ExtError::NotAWasm`] unless URL ends in `.wasm`.
/// [`ExtError::OpaqueUrl`] for query/fragment (relative reference drops queries,
/// access tokens would silently disappear from sibling URLs).
fn wanted(url: &str, allow_unsigned: bool) -> Result<Vec<(String, String)>, ExtError> {
    let parsed = url::Url::parse(url).map_err(|_| ExtError::NotAWasm {
        path: url.to_owned(),
    })?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ExtError::OpaqueUrl {
            url: url.to_owned(),
        });
    }
    let file = parsed
        .path_segments()
        .and_then(Iterator::last)
        .unwrap_or_default();
    let Some(name) = file.strip_suffix(&format!(".{COMPONENT_EXT}")) else {
        return Err(ExtError::NotAWasm {
            path: url.to_owned(),
        });
    };
    if name.is_empty() {
        return Err(ExtError::NotAWasm {
            path: url.to_owned(),
        });
    }

    let component = format!("{name}.{COMPONENT_EXT}");
    let manifest = format!("{name}.manifest.toml");
    let mut files = vec![component.clone(), manifest.clone()];
    if !allow_unsigned {
        files.push(format!("{component}.minisig"));
        files.push(format!("{manifest}.minisig"));
    }
    files
        .into_iter()
        .map(|file| {
            parsed
                .join(&file)
                .map(|resolved| (resolved.to_string(), file))
                .map_err(|_| ExtError::NotAWasm {
                    path: url.to_owned(),
                })
        })
        .collect()
}

/// Policy-bound HTTP client for remote installs (single construction, no weak pass).
///
/// Every redirect is re-checked (#107); `Authorization` does not survive hops.
#[must_use = "the policy-bound client does nothing until it is handed to an installer"]
pub fn policy_bound_http() -> crate::route::HttpFn {
    Box::new(|method, url, headers, body, timeout| {
        crate::http::fetch_within(
            &crate::egress::EgressPolicy::public_only(),
            method,
            url,
            headers,
            body,
            timeout,
        )
    })
}

/// Fetch a component and companions, then install as if on-disk (download adds
/// bytes location, nothing else; goes through [`install`] unchanged).
///
/// `http` is injected for testability and should be policy-bound (checked per hop);
/// [`check_remote`] checks all URLs first so the operator gets a clear message.
///
/// # Errors
/// [`ExtError::RefusedByPolicy`] if policy denies, [`ExtError::Fetch`] on
/// request failure/non-200, [`wanted`] errors, or [`install`] refusals.
pub fn install_from_url(
    dir: &Path,
    url: &str,
    checks: &Checks,
    http: &crate::route::HttpFn,
) -> Result<Installed, ExtError> {
    let wanted = wanted(url, checks.allow_unsigned)?;
    let incoming = staging_dir();
    let _ = std::fs::remove_dir_all(&incoming);
    crate::wasm_cache::ensure_private_dir(&incoming).map_err(|source| ExtError::Staging {
        path: incoming.display().to_string(),
        source,
    })?;

    let outcome = fetch_all(&incoming, &wanted, http)
        .and_then(|component| install(dir, &incoming.join(component), checks));
    let _ = std::fs::remove_dir_all(&incoming);
    outcome
}

/// Counter for distinct staging dirs per call (macOS clock granularity: #165).
static NEXT_STAGING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Staging dir per call (not per process: #168). Per-process would wipe
/// neighbours' half-fetched files.
fn staging_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "jk-ext-fetch-{}-{}",
        std::process::id(),
        NEXT_STAGING.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// Fetch wanted files into `incoming`, return component name.
fn fetch_all(
    incoming: &Path,
    wanted: &[(String, String)],
    http: &crate::route::HttpFn,
) -> Result<String, ExtError> {
    for (remote, file) in wanted {
        check_remote(remote)?;
        let response = http("GET", remote, &[], None, 0).map_err(|err| ExtError::Fetch {
            url: remote.clone(),
            detail: format!("{err:?}"),
        })?;
        if response.status != 200 {
            return Err(ExtError::Fetch {
                url: remote.clone(),
                detail: format!("answered {}", response.status),
            });
        }
        std::fs::write(incoming.join(file), &response.body).map_err(|source| {
            ExtError::Staging {
                path: incoming.join(file).display().to_string(),
                source,
            }
        })?;
    }
    wanted
        .first()
        .map(|(_, file)| file.clone())
        .ok_or(ExtError::Fetch {
            url: String::new(),
            detail: "nothing to fetch".to_owned(),
        })
}

/// File's detached signature path (`<file>.minisig`).
fn signature_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(".minisig");
    PathBuf::from(name)
}

/// Verify `content` (staged bytes) against `.minisig` (beside operator's original).
/// Prehashed signatures only: legacy format (raw file) not accepted (no project usage).
fn verify_signature(
    content: &Path,
    beside: &Path,
    shown: &str,
    keys: &[String],
) -> Result<(), ExtError> {
    let signature = signature_path(beside);
    if !signature.exists() {
        return Err(ExtError::Unsigned {
            path: shown.to_owned(),
        });
    }
    let text = std::fs::read_to_string(&signature).map_err(|err| ExtError::MalformedSignature {
        path: shown.to_owned(),
        detail: err.to_string(),
    })?;
    let parsed =
        minisign_verify::Signature::decode(&text).map_err(|err| ExtError::MalformedSignature {
            path: shown.to_owned(),
            detail: err.to_string(),
        })?;
    let bytes = std::fs::read(content).map_err(|err| ExtError::MalformedSignature {
        path: shown.to_owned(),
        detail: err.to_string(),
    })?;

    for (index, key) in keys.iter().enumerate() {
        let key = minisign_verify::PublicKey::from_base64(key.trim()).map_err(|err| {
            ExtError::MalformedTrustedKey {
                index,
                detail: err.to_string(),
            }
        })?;
        if key.verify(&bytes, &parsed, false).is_ok() {
            return Ok(());
        }
    }
    Err(ExtError::Untrusted {
        path: shown.to_owned(),
        keys: keys.len(),
    })
}

/// Whether text looks like a SHA-256 digest (64 hex chars; errors reported as
/// malformed before comparison, not mismatch).
fn looks_like_a_digest(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Install staging dir (inside `ext/` so final move is same-filesystem rename).
const STAGING: &str = ".staging";

/// Component manifest status (three states: present, absent, broken).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// Manifest reads.
    Present(Manifest),
    /// No manifest file beside component.
    Absent,
    /// Manifest present but unusable (reason included).
    Broken(String),
}

/// Staged component and its manifest status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// File stem (name for [`remove`]).
    pub name: String,
    /// Component file.
    pub component: PathBuf,
    /// Manifest or reason there isn't one.
    pub declaration: Declaration,
}

/// Extension operation failure.
#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    /// Extension directory unreadable.
    #[error("reading {path}")]
    Unreadable {
        /// Directory path.
        path: String,
        /// I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file stem names no category this runtime dispatches (#222).
    ///
    /// Refused before a byte moves: a component that could never be called should not reach `ext/`.
    #[error("{reason}")]
    UncategorisedStem {
        /// What `categorise` said, which names the categories that exist.
        reason: String,
    },
    /// Nothing by that name is staged.
    #[error("no component named {name} in {dir}")]
    NotInstalled {
        /// The name that was asked for.
        name: String,
        /// Where it was looked for.
        dir: String,
    },
    /// A file existed and could not be deleted.
    #[error("removing {path}")]
    Undeletable {
        /// The file that could not be removed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A remote source the egress policy does not permit.
    #[error(
        "{url} is not a destination the egress policy permits ({detail}): only public \
         addresses, and loopback, private, link-local and unique-local are refused"
    )]
    RefusedByPolicy {
        /// The URL as given.
        url: String,
        /// What the policy said, for the operator to match against the rule.
        detail: String,
    },
    /// A remote URL carries a query or fragment, which sibling derivation
    /// would silently drop.
    #[error(
        "{url} carries a query or fragment. The manifest and signature URLs are \
         derived from this one, and a relative reference drops a query — so they \
         would be requested without it. Publish the files at plain paths, or fetch \
         and install them separately."
    )]
    OpaqueUrl {
        /// The URL as given.
        url: String,
    },
    /// A remote file could not be fetched.
    #[error("fetching {url}: {detail}")]
    Fetch {
        /// The URL that failed.
        url: String,
        /// What went wrong.
        detail: String,
    },
    /// The path given to `install` is not a `.wasm` file.
    #[error("{path} is not a .wasm component file")]
    NotAWasm {
        /// The path as given.
        path: String,
    },
    /// The path given to `install` does not exist.
    #[error("{path} does not exist")]
    SourceMissing {
        /// The path as given.
        path: String,
    },
    /// Something of that name is already staged.
    #[error("{name} is already installed in {dir}; remove it first")]
    AlreadyInstalled {
        /// The name that collided.
        name: String,
        /// Where it collided.
        dir: String,
    },
    /// The expected digest is not a SHA-256 digest.
    #[error("{given:?} is not a SHA-256 digest (expected 64 hex characters, got {length})")]
    MalformedDigest {
        /// What was passed.
        given: String,
        /// Its length, since a truncated paste is the usual cause.
        length: usize,
    },
    /// The component's bytes are not the ones the digest describes.
    #[error(
        "{path} does not match the expected digest\n  expected {expected}\n  actual   {actual}"
    )]
    DigestMismatch {
        /// The component that was rejected.
        path: String,
        /// The digest that was asked for.
        expected: String,
        /// The digest the bytes actually have.
        actual: String,
    },
    /// `config.yaml` could not be read or parsed.
    #[error("reading {path}: {detail}")]
    Config {
        /// The config file that could not be used.
        path: String,
        /// What the config crate objected to.
        detail: String,
    },
    /// `registry.trusted-keys` is not a list of strings.
    #[error("`registry.trusted-keys` must be a list of minisign public keys")]
    TrustedKeysNotAList,
    /// A configured trusted key cannot be read as a minisign public key.
    #[error("`registry.trusted-keys` entry {index} is not a minisign public key: {detail}")]
    MalformedTrustedKey {
        /// Which entry, so the operator can find it in `config.yaml`.
        index: usize,
        /// What the parser objected to.
        detail: String,
    },
    /// No signature beside the file, and the signature was not waived.
    #[error(
        "no signature at {path}.minisig. Sign it, name the key in \
         `registry.trusted-keys`, or pass --allow-unsigned with --sha256"
    )]
    Unsigned {
        /// The file that arrived without a signature.
        path: String,
    },
    /// A `.minisig` is there and cannot be parsed.
    #[error("the signature at {path}.minisig cannot be read: {detail}")]
    MalformedSignature {
        /// The file whose signature is unreadable.
        path: String,
        /// What the parser objected to.
        detail: String,
    },
    /// No trusted key verifies this signature.
    ///
    /// One error for wrong key and wrong bytes on purpose: an installer cannot tell them apart honestly.
    #[error(
        "the signature for {path} is not valid under any key in \
         `registry.trusted-keys` ({keys} configured)"
    )]
    Untrusted {
        /// The file whose signature did not verify.
        path: String,
        /// How many keys were tried — `0` is the usual cause, and says so.
        keys: usize,
    },
    /// `--allow-unsigned` without the digest that must accompany it.
    #[error(
        "--allow-unsigned requires --sha256 <hex>: waiving the signature leaves the \
         digest as the only evidence about these bytes"
    )]
    DigestRequiredWhenUnsigned,
    /// The file is not a WebAssembly component (a core module, or not wasm).
    #[error("{path} is not a WebAssembly component")]
    NotAComponent {
        /// The file that failed to compile.
        path: String,
        /// Wasmtime's complaint, boxed as `CoreError::Load` boxes it.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// No manifest beside the component being installed.
    #[error("no manifest beside {path}; a component nobody can inspect is not installable")]
    NoManifest {
        /// The component that arrived alone.
        path: String,
    },
    /// A manifest is there and cannot be used.
    #[error("the manifest beside {path} cannot be used")]
    ManifestUnusable {
        /// The component whose manifest is broken.
        path: String,
        /// Why it could not be read.
        #[source]
        source: crate::manifest::ManifestError,
    },
    /// Built against an interface package this host does not speak.
    #[error("{path} was built against jan-klod:interfaces@{theirs}, and this host speaks {ours}")]
    ApiMismatch {
        /// The component that does not fit.
        path: String,
        /// The version its manifest declares.
        theirs: String,
        /// The version this host was built against.
        ours: String,
    },
    /// The component imports host interfaces its manifest does not declare.
    #[error("{path} imports {interfaces}, which its manifest does not declare")]
    UnderDeclared {
        /// The component that asks for more than it admits to.
        path: String,
        /// The interfaces it did not declare.
        interfaces: String,
    },
    /// Staging failed — copying in, or making the directory.
    #[error("staging {path}")]
    Staging {
        /// What was being written.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The final move into `ext/` failed.
    #[error("moving {path} into place")]
    Landing {
        /// What was being moved.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Every staged component in `dir`, sorted by name.
///
/// A single unusable manifest is reported as [`Declaration::Broken`] instead of failing the listing.
///
/// # Errors
/// [`ExtError::Unreadable`] if `dir` cannot be read at all. A directory that
/// [`ExtError::Unreadable`] if `dir` cannot be read; a missing directory lists as empty.
pub fn list(dir: &Path) -> Result<Vec<Installed>, ExtError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir).map_err(|source| ExtError::Unreadable {
        path: dir.display().to_string(),
        source,
    })?;

    let mut staged = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ExtError::Unreadable {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(COMPONENT_EXT) {
            continue;
        }
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let declaration = match Manifest::beside(&path) {
            Ok(Some(manifest)) => Declaration::Present(manifest),
            Ok(None) => Declaration::Absent,
            Err(err) => Declaration::Broken(err.to_string()),
        };
        staged.push(Installed {
            name,
            component: path,
            declaration,
        });
    }
    staged.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(staged)
}

/// What [`remove`] deleted.
///
/// An enum so "neither" cannot be expressed; [`remove`] refuses it with [`ExtError::NotInstalled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removed {
    /// The component and its manifest, as a healthy install has both.
    Both,
    /// A component that had no manifest — the boot path would have refused it.
    ComponentOnly,
    /// A manifest with no component beside it: an orphan declaration.
    ManifestOnly,
}

/// Delete a staged component **and** its manifest.
///
/// Both, always: a lone manifest is an orphan declaration that `list` would believe. Reports the
/// pair actually deleted.
///
/// # Errors
/// [`ExtError::NotInstalled`] when neither file exists; [`ExtError::Undeletable`] if one cannot be deleted.
pub fn remove(dir: &Path, name: &str) -> Result<Removed, ExtError> {
    let component = dir.join(format!("{name}.{COMPONENT_EXT}"));
    let manifest = manifest_path(&component);

    let had_component = component.exists();
    let had_manifest = manifest.exists();
    if !had_component && !had_manifest {
        return Err(ExtError::NotInstalled {
            name: name.to_owned(),
            dir: dir.display().to_string(),
        });
    }

    if had_component {
        std::fs::remove_file(&component).map_err(|source| ExtError::Undeletable {
            path: component.display().to_string(),
            source,
        })?;
    }
    if had_manifest {
        std::fs::remove_file(&manifest).map_err(|source| ExtError::Undeletable {
            path: manifest.display().to_string(),
            source,
        })?;
    }
    Ok(match (had_component, had_manifest) {
        (true, true) => Removed::Both,
        (true, false) => Removed::ComponentOnly,
        (false, _) => Removed::ManifestOnly,
    })
}

/// Install the component at `source` into `dir`, or refuse and change nothing.
///
/// Everything is assembled and checked in `ext/.staging/<name>/`; only a component that passed every
/// check moves into `ext/`, so a boot never sees anything half-verified.
///
/// Integrity is decided by the same [`crate::inspect`] boot uses, so install cannot accept what boot
/// refuses. Provenance (digest, signature) is checked per [`Checks`].
///
/// # Errors
/// A distinct [`ExtError`] per refusal; on any, `dir` is unchanged and staging is removed.
pub fn install(dir: &Path, source: &Path, checks: &Checks) -> Result<Installed, ExtError> {
    if checks.allow_unsigned && checks.sha256.is_none() {
        return Err(ExtError::DigestRequiredWhenUnsigned);
    }
    if !source.exists() {
        return Err(ExtError::SourceMissing {
            path: source.display().to_string(),
        });
    }
    if source.extension().and_then(|e| e.to_str()) != Some(COMPONENT_EXT) {
        return Err(ExtError::NotAWasm {
            path: source.display().to_string(),
        });
    }
    let Some(name) = source.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return Err(ExtError::NotAWasm {
            path: source.display().to_string(),
        });
    };
    crate::categorise(&name).map_err(|reason| ExtError::UncategorisedStem { reason })?;

    let landing = dir.join(format!("{name}.{COMPONENT_EXT}"));
    if landing.exists() {
        return Err(ExtError::AlreadyInstalled {
            name,
            dir: dir.display().to_string(),
        });
    }

    let staging = dir.join(STAGING).join(&name);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|source| ExtError::Staging {
        path: staging.display().to_string(),
        source,
    })?;

    let outcome = stage_and_check(&staging, source, &name, checks);
    let _ = std::fs::remove_dir_all(&staging);
    let _ = std::fs::remove_dir(dir.join(STAGING));
    outcome
}

/// Check `staged` against an expected digest, if one was given.
///
/// Checks the staged copy: those are the bytes that would land.
fn verify_digest(staged: &Path, shown: &Path, expected: Option<&str>) -> Result<(), ExtError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if !looks_like_a_digest(expected) {
        return Err(ExtError::MalformedDigest {
            given: expected.to_owned(),
            length: expected.len(),
        });
    }
    let bytes = std::fs::read(staged).map_err(|err| ExtError::Staging {
        path: staged.display().to_string(),
        source: err,
    })?;
    let actual = hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ExtError::DigestMismatch {
            path: shown.display().to_string(),
            expected: expected.to_ascii_lowercase(),
            actual,
        })
    }
}

/// Compile the staged component and cross-check its manifest, mapping each
/// verdict to a refusal.
///
/// Compiling is the "is this a component" check, with the same engine as boot.
fn check_structure(staged: &Path, shown: &Path) -> Result<(), ExtError> {
    let engine = Engine::default();
    let component =
        Component::from_file(&engine, staged).map_err(|err| ExtError::NotAComponent {
            path: shown.display().to_string(),
            source: err.into(),
        })?;
    let inspected =
        inspect(&component, &engine, staged).map_err(|err| ExtError::ManifestUnusable {
            path: shown.display().to_string(),
            source: err,
        })?;
    match inspected.verdict {
        Verdict::Consistent => Ok(()),
        Verdict::NoManifest => Err(ExtError::NoManifest {
            path: shown.display().to_string(),
        }),
        Verdict::ApiMismatch { theirs } => Err(ExtError::ApiMismatch {
            path: shown.display().to_string(),
            theirs,
            ours: crate::manifest::API_VERSION.to_owned(),
        }),
        Verdict::UnderDeclared { interfaces } => Err(ExtError::UnderDeclared {
            path: shown.display().to_string(),
            interfaces: interfaces.join(", "),
        }),
    }
}

/// Copy into `staging`, check, and move into place on success.
fn stage_and_check(
    staging: &Path,
    source: &Path,
    name: &str,
    checks: &Checks,
) -> Result<Installed, ExtError> {
    let dir = staging
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| ExtError::Staging {
            path: staging.display().to_string(),
            source: std::io::Error::other("staging path has no extension directory above it"),
        })?;

    let staged_component = staging.join(format!("{name}.{COMPONENT_EXT}"));
    std::fs::copy(source, &staged_component).map_err(|err| ExtError::Staging {
        path: staged_component.display().to_string(),
        source: err,
    })?;

    verify_digest(&staged_component, source, checks.sha256.as_deref())?;

    let source_manifest = manifest_path(source);
    if !source_manifest.exists() {
        return Err(ExtError::NoManifest {
            path: source.display().to_string(),
        });
    }
    let staged_manifest = manifest_path(&staged_component);
    std::fs::copy(&source_manifest, &staged_manifest).map_err(|err| ExtError::Staging {
        path: staged_manifest.display().to_string(),
        source: err,
    })?;

    if !checks.allow_unsigned {
        verify_signature(
            &staged_component,
            source,
            &source.display().to_string(),
            &checks.trusted_keys,
        )?;
        verify_signature(
            &staged_manifest,
            &source_manifest,
            &source_manifest.display().to_string(),
            &checks.trusted_keys,
        )?;
    }

    check_structure(&staged_component, source)?;

    let landed_manifest = manifest_path(&dir.join(format!("{name}.{COMPONENT_EXT}")));
    std::fs::rename(&staged_manifest, &landed_manifest).map_err(|err| ExtError::Landing {
        path: landed_manifest.display().to_string(),
        source: err,
    })?;
    let landed_component = dir.join(format!("{name}.{COMPONENT_EXT}"));
    if let Err(err) = std::fs::rename(&staged_component, &landed_component) {
        let _ = std::fs::remove_file(&landed_manifest);
        return Err(ExtError::Landing {
            path: landed_component.display().to_string(),
            source: err,
        });
    }

    Ok(Installed {
        name: name.to_owned(),
        component: landed_component,
        declaration: match Manifest::beside(&dir.join(format!("{name}.{COMPONENT_EXT}"))) {
            Ok(Some(manifest)) => Declaration::Present(manifest),
            Ok(None) => Declaration::Absent,
            Err(err) => Declaration::Broken(err.to_string()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{install, list, remove, staging_dir, Checks, Declaration, ExtError, Removed};
    use std::path::{Path, PathBuf};

    /// Removes the directory on drop, panic or not.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("jk-ext-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creates a temp dir");
        TempDir(dir)
    }

    /// A component file and, optionally, the manifest beside it.
    fn stage(dir: &Path, name: &str, manifest: Option<&str>) {
        std::fs::write(dir.join(format!("{name}.wasm")), b"\0asm").expect("writes a component");
        if let Some(body) = manifest {
            std::fs::write(dir.join(format!("{name}.manifest.toml")), body)
                .expect("writes a manifest");
        }
    }

    fn manifest_for(name: &str, capabilities: &str) -> String {
        format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\napi-version = \"0.1.0\"\n\
             kind = \"tool\"\ndescription = \"\"\ncapabilities = [{capabilities}]\n"
        )
    }

    #[test]
    fn an_absent_directory_lists_as_empty_rather_than_failing() {
        let dir = temp_dir("absent");
        let missing = dir.0.join("never-created");
        assert_eq!(list(&missing).expect("nothing staged is not an error"), []);
    }

    #[test]
    fn components_are_listed_by_name_with_what_they_declare() {
        let dir = temp_dir("list");
        stage(&dir.0, "tool-zed", Some(&manifest_for("tool-zed", "")));
        stage(
            &dir.0,
            "tool-abc",
            Some(&manifest_for("tool-abc", "\"host-fs\"")),
        );
        std::fs::write(dir.0.join("notes.txt"), "ignored").expect("writes a stray file");

        let staged = list(&dir.0).expect("lists");
        let names: Vec<&str> = staged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["tool-abc", "tool-zed"],
            "sorted, and only the .wasm"
        );

        let Declaration::Present(manifest) = &staged[0].declaration else {
            panic!("tool-abc has a readable manifest: {:?}", staged[0])
        };
        assert_eq!(manifest.capabilities, ["host-fs"]);

        let Declaration::Present(empty) = &staged[1].declaration else {
            panic!("tool-zed has a readable manifest")
        };
        assert!(
            empty.capabilities.is_empty(),
            "an empty list is a claim, not an absence"
        );
    }

    /// The two unhappy manifests are distinguished because the fixes differ.
    #[test]
    fn a_missing_manifest_and_an_unusable_one_are_different_states() {
        let dir = temp_dir("declarations");
        stage(&dir.0, "tool-bare", None);
        stage(&dir.0, "tool-broken", Some("this is not toml{{{"));

        let staged = list(&dir.0).expect("one bad manifest does not fail the listing");
        assert_eq!(staged.len(), 2, "both are still reported: {staged:?}");
        assert_eq!(staged[0].declaration, Declaration::Absent);
        let Declaration::Broken(reason) = &staged[1].declaration else {
            panic!("a present-but-unparseable manifest is Broken: {staged:?}")
        };
        assert!(
            reason.contains("TOML"),
            "the reason names what is wrong: {reason}"
        );
    }

    #[test]
    fn removing_takes_the_component_and_the_manifest() {
        let dir = temp_dir("remove");
        stage(&dir.0, "tool-fs", Some(&manifest_for("tool-fs", "")));
        assert_eq!(remove(&dir.0, "tool-fs").expect("removes"), Removed::Both);
        assert!(!dir.0.join("tool-fs.wasm").exists());
        assert!(
            !dir.0.join("tool-fs.manifest.toml").exists(),
            "a manifest left behind is an orphan declaration the next list believes"
        );
        assert_eq!(list(&dir.0).expect("lists"), []);
    }

    /// An orphan manifest is removable on its own.
    #[test]
    fn each_file_can_be_removed_without_the_other() {
        let dir = temp_dir("partial");
        stage(&dir.0, "tool-bare", None);
        assert_eq!(
            remove(&dir.0, "tool-bare").expect("removes"),
            Removed::ComponentOnly
        );

        std::fs::write(
            dir.0.join("tool-ghost.manifest.toml"),
            manifest_for("tool-ghost", ""),
        )
        .expect("writes an orphan manifest");
        assert_eq!(
            remove(&dir.0, "tool-ghost").expect("removes"),
            Removed::ManifestOnly
        );
    }

    /// Checks that get past the signature gate for tests about something else: fabricated bytes take
    /// the waiver plus a computed digest.
    fn waived(file: &Path) -> Checks {
        let bytes = std::fs::read(file).expect("reads the file being waived");
        Checks {
            sha256: Some(super::hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes))),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        }
    }

    /// Every file in `dir` with its bytes: a refusal must leave `ext/` byte-identical.
    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .expect("reads the directory")
            .map(|entry| {
                let path = entry.expect("an entry").path();
                let name = path
                    .strip_prefix(dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let bytes = if path.is_dir() {
                    Vec::new()
                } else {
                    std::fs::read(&path).expect("reads a file")
                };
                (name, bytes)
            })
            .collect();
        files.sort();
        files
    }

    /// A refusal leaves nothing behind: no component, manifest or staging directory.
    fn refuses_and_changes_nothing(tag: &str, prepare: impl Fn(&Path) -> PathBuf) -> ExtError {
        let dir = temp_dir(tag);
        let ext = dir.0.join("ext");
        std::fs::create_dir_all(&ext).expect("creates ext/");
        stage(
            &ext,
            "tool-present",
            Some(&manifest_for("tool-present", "")),
        );
        let before = snapshot(&ext);

        let source = prepare(&dir.0);
        let err = install(&ext, &source, &waived(&source)).expect_err("must refuse");

        assert_eq!(
            snapshot(&ext),
            before,
            "{tag}: ext/ must be byte-identical after a refused install"
        );
        assert!(
            !ext.join(super::STAGING).exists(),
            "{tag}: the staging directory must not survive a refusal"
        );
        err
    }

    #[test]
    fn a_file_that_is_not_a_component_is_refused_and_leaves_nothing() {
        let err = refuses_and_changes_nothing("not-component", |root| {
            let source = root.join("tool-junk.wasm");
            std::fs::write(&source, b"not wasm at all").expect("writes junk");
            std::fs::write(
                root.join("tool-junk.manifest.toml"),
                manifest_for("tool-junk", ""),
            )
            .expect("writes a manifest");
            source
        });
        assert!(
            matches!(err, ExtError::NotAComponent { .. }),
            "arbitrary bytes are refused as not-a-component: {err:?}"
        );
    }

    #[test]
    fn a_component_with_no_manifest_is_refused_and_leaves_nothing() {
        let err = refuses_and_changes_nothing("no-manifest", |root| {
            let source = root.join("tool-alone.wasm");
            std::fs::write(&source, b"\0asm").expect("writes a component");
            source
        });
        assert!(
            matches!(err, ExtError::NoManifest { .. }),
            "a component arriving alone is refused before it is compiled: {err:?}"
        );
    }

    /// The refusals must be distinguishable, hence one variant each.
    #[test]
    fn each_refusal_says_something_different() {
        let dir = temp_dir("distinct");
        let ext = dir.0.join("ext");
        std::fs::create_dir_all(&ext).expect("creates ext/");

        let absent =
            install(&ext, &dir.0.join("nowhere.wasm"), &Checks::default()).expect_err("refuses");
        let not_wasm = {
            let path = dir.0.join("notes.txt");
            std::fs::write(&path, b"x").expect("writes");
            install(&ext, &path, &waived(&path)).expect_err("refuses")
        };
        let alone = {
            let path = dir.0.join("tool-alone.wasm");
            std::fs::write(&path, b"\0asm").expect("writes");
            install(&ext, &path, &waived(&path)).expect_err("refuses")
        };
        let junk = {
            let path = dir.0.join("tool-junk.wasm");
            std::fs::write(&path, b"nope").expect("writes");
            std::fs::write(
                dir.0.join("tool-junk.manifest.toml"),
                manifest_for("tool-junk", ""),
            )
            .expect("writes");
            install(&ext, &path, &waived(&path)).expect_err("refuses")
        };

        let messages = [
            absent.to_string(),
            not_wasm.to_string(),
            alone.to_string(),
            junk.to_string(),
        ];
        let mut unique = messages.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            messages.len(),
            "each refusal needs its own message: {messages:?}"
        );
    }

    /// A malformed digest is its own refusal, not a mismatch.
    #[test]
    fn a_digest_that_is_not_a_digest_is_refused_as_malformed() {
        let dir = temp_dir("malformed-digest");
        let ext = dir.0.join("ext");
        std::fs::create_dir_all(&ext).expect("creates ext/");
        let source = dir.0.join("tool-x.wasm");
        std::fs::write(&source, b"\0asm").expect("writes a component");

        for bad in [
            "016481a6eb79d32a82af4a1f7d49af56b11af9431870a857016e4696e62aca2",
            "z16481a6eb79d32a82af4a1f7d49af56b11af9431870a857016e4696e62aca26",
            "",
            "sha256:016481a6",
        ] {
            let err = install(
                &ext,
                &source,
                &Checks {
                    sha256: Some(bad.to_owned()),
                    trusted_keys: Vec::new(),
                    allow_unsigned: true,
                },
            )
            .expect_err("must refuse");
            assert!(
                matches!(err, ExtError::MalformedDigest { .. }),
                "{bad:?} is malformed, not a mismatch: {err:?}"
            );
        }
        assert!(!ext.join(super::STAGING).exists());
        assert_eq!(list(&ext).expect("lists"), []);
    }

    #[test]
    fn installing_over_something_already_there_is_refused() {
        let dir = temp_dir("collide");
        let ext = dir.0.join("ext");
        std::fs::create_dir_all(&ext).expect("creates ext/");
        stage(&ext, "tool-fs", Some(&manifest_for("tool-fs", "")));
        let before = snapshot(&ext);

        let source = dir.0.join("tool-fs.wasm");
        std::fs::write(&source, b"\0asm").expect("writes a component");
        let err = install(&ext, &source, &waived(&source)).expect_err("must refuse");
        assert!(
            matches!(err, ExtError::AlreadyInstalled { .. }),
            "replacing silently would discard the component the config was verified against: {err:?}"
        );
        assert_eq!(snapshot(&ext), before, "and the original is untouched");
    }

    #[test]
    fn removing_something_that_is_not_there_is_refused_by_name() {
        let dir = temp_dir("absent-remove");
        let Err(ExtError::NotInstalled { name, .. }) = remove(&dir.0, "tool-nope") else {
            panic!("nothing to remove must be an error, not a silent success")
        };
        assert_eq!(name, "tool-nope");
    }

    /// #168: the staging directory is per call and private.
    ///
    /// Drives the naming and mode directly, since neither shows in `install_from_url`'s return value.
    /// The old per-pid name made concurrent installs delete each other's half-fetched files.
    #[test]
    fn two_installs_in_one_process_do_not_share_a_staging_directory() {
        let first = staging_dir();
        let second = staging_dir();
        assert_ne!(
            first, second,
            "two calls staged into one directory, and each wipes it at both \
             ends — the second install would delete the first's downloads"
        );

        for dir in [&first, &second] {
            crate::wasm_cache::ensure_private_dir(dir).expect("staging dir is creatable");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&first)
                .expect("created")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o700,
                "staging is {:o}, so another user on this machine can read an \
                 unverified component and its signature while they are being \
                 checked",
                mode & 0o777
            );
        }
        for dir in [first, second] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
