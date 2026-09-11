//! `ext install` against real components.
//!
//! The refusals that need only bytes — not a component, no manifest, already
//! installed — are unit tests in `core::ext`, where they are cheaper. What
//! cannot be tested there is anything requiring a *real* component: that a
//! valid one lands and is loadable afterwards, and that a manifest which
//! disagrees with the component's actual imports is refused. Both need imports
//! to disagree *about*, and fabricated bytes have none.

use std::path::{Path, PathBuf};

use jan_klod_core::ext::{self, Declaration, ExtError};

use crate::common;

/// A scratch `ext/` and a place to put sources, removed on drop.
struct Scratch {
    root: PathBuf,
    ext: PathBuf,
    incoming: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch(tag: &str) -> Scratch {
    let root = std::env::temp_dir().join(format!("jk-ext-install-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ext = root.join("ext");
    let incoming = root.join("incoming");
    std::fs::create_dir_all(&ext).expect("creates a scratch ext/");
    std::fs::create_dir_all(&incoming).expect("creates an incoming dir");
    Scratch {
        root,
        ext,
        incoming,
    }
}

/// Copy a staged guest and its manifest into `incoming`, returning the
/// component's path — an installable pair, exactly as a release would ship it.
fn offer(scratch: &Scratch, guest: &str) -> PathBuf {
    let staged = common::repo_root().join("ext");
    let component = scratch.incoming.join(format!("{guest}.wasm"));
    std::fs::copy(staged.join(format!("{guest}.wasm")), &component).expect("copies the component");
    std::fs::copy(
        staged.join(format!("{guest}.manifest.toml")),
        scratch.incoming.join(format!("{guest}.manifest.toml")),
    )
    .expect("copies the manifest");
    component
}

/// The SHA-256 of a file, **computed by something other than the code under
/// test**.
///
/// `install` hashes with `sha2` and renders the digest with a hand-written hex
/// helper. A test that produced its expected value the same way would agree
/// with itself no matter how wrong that helper was — a mis-ordered or
/// zero-padded encoding would be wrong *consistently*, so every comparison
/// would still pass. So the expectation comes from the system's own tool, which
/// shares no code with the host: `sha256sum` on Linux, `shasum -a 256` on
/// macOS.
///
/// Returns `None` when neither exists, and the caller skips — a missing tool
/// must not be reported as a digest failure.
fn sha256_of(path: &Path) -> Option<String> {
    let candidates: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (program, flags) in candidates {
        if !common::tool_available(program) {
            continue;
        }
        let output = std::process::Command::new(program)
            .args(flags)
            .arg(path)
            .output()
            .expect("the digest tool runs");
        assert!(
            output.status.success(),
            "{program} failed on {}",
            path.display()
        );
        let text = String::from_utf8(output.stdout).expect("a digest is ASCII");
        // Both tools print `<hex>  <path>`.
        return text.split_whitespace().next().map(str::to_ascii_lowercase);
    }
    None
}

/// An offer that is properly signed, plus the checks that accept it.
///
/// The default for every test about something *other* than signatures: signed
/// is the installer's default, so a structural test that skipped it would be
/// refused before reaching the check it cares about.
fn signed_offer(scratch: &Scratch, guest: &str) -> (PathBuf, ext::Checks) {
    let component = offer(scratch, guest);
    let signer = crate::common::minisig::Signer::new();
    signer.sign(&component);
    signer.sign(&scratch.incoming.join(format!("{guest}.manifest.toml")));
    let checks = ext::Checks {
        sha256: None,
        trusted_keys: vec![signer.public_key_base64()],
        allow_unsigned: false,
    };
    (component, checks)
}

/// Re-sign an offer whose files were edited after `signed_offer` signed them.
///
/// The structural tests rewrite a manifest *after* staging it, which invalidates
/// its signature — so without this they would be refused for the wrong reason
/// and would pass while proving nothing about the structural check.
fn resign(scratch: &Scratch, guest: &str) -> ext::Checks {
    let signer = crate::common::minisig::Signer::new();
    signer.sign(&scratch.incoming.join(format!("{guest}.wasm")));
    signer.sign(&scratch.incoming.join(format!("{guest}.manifest.toml")));
    ext::Checks {
        sha256: None,
        trusted_keys: vec![signer.public_key_base64()],
        allow_unsigned: false,
    }
}

/// Every name in a directory, sorted — enough to assert "nothing landed".
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("reads the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_valid_component_lands_with_its_manifest_and_is_then_loadable() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("valid");
    let (source, checks) = signed_offer(&scratch, "tool-fs");

    let installed =
        ext::install(&scratch.ext, &source, &checks).expect("a real component installs");
    assert_eq!(installed.name, "tool-fs");

    // Both files, and nothing else — no staging directory left over.
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair lands together and the staging directory is gone"
    );

    // The declaration it reports is the one now on disk.
    let Declaration::Present(manifest) = &installed.declaration else {
        panic!("a component that installed has a readable manifest: {installed:?}")
    };
    assert_eq!(manifest.capabilities, ["host-fs"]);

    // And `list` — a separate reader — agrees about what landed.
    let listed = ext::list(&scratch.ext).expect("lists");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].declaration, installed.declaration);

    // The claim worth more than either: what installed is *loadable*. Compiling
    // it is what the boot path does, so this is the check that an install
    // cannot succeed on something the runtime would then reject.
    let engine = wasmtime::Engine::default();
    wasmtime::component::Component::from_file(&engine, scratch.ext.join("tool-fs.wasm"))
        .expect("what install accepted, the runtime can load");
}

/// The refusal that needs a real component: a manifest which does not admit to
/// what the component imports. This is the check that makes an install worth
/// more than a checksum — a component's declaration is what an operator reads
/// before granting anything, so one that under-declares must not land.
#[test]
fn a_manifest_that_under_declares_is_refused_and_nothing_lands() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("under-declared");
    let source = offer(&scratch, "tool-fs");

    // tool-fs really imports `host-fs`; say it needs nothing.
    let manifest = scratch.incoming.join("tool-fs.manifest.toml");
    let text = std::fs::read_to_string(&manifest).expect("reads the manifest");
    assert!(
        text.contains("\"host-fs\""),
        "the fixture removes this line, so it has to be there: {text}"
    );
    std::fs::write(
        &manifest,
        text.replace("\"host-fs\",", "").replace("\"host-fs\"", ""),
    )
    .expect("rewrites the manifest");

    let checks = resign(&scratch, "tool-fs");
    let err = ext::install(&scratch.ext, &source, &checks).expect_err("must refuse");
    let ExtError::UnderDeclared { interfaces, .. } = &err else {
        panic!("an under-declaring manifest is refused as such: {err:?}")
    };
    assert!(
        interfaces.contains("host-fs"),
        "the refusal names the interface that was hidden: {interfaces}"
    );
    assert_eq!(
        names(&scratch.ext),
        Vec::<String>::new(),
        "nothing landed, and no staging directory survived"
    );
}

/// The digest check, against a real component: the right digest installs, and
/// one tampered byte is refused with **both** digests named.
///
/// Tampering by flipping a byte rather than by writing junk, because junk would
/// also fail to compile — and then the test would pass whether the digest was
/// checked or not. A component that is still perfectly valid wasm, with one
/// byte changed, can only be caught by the digest.
#[test]
fn a_correct_digest_installs_and_a_tampered_byte_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("digest");
    let source = offer(&scratch, "tool-fs");
    let Some(digest) = sha256_of(&source) else {
        // No independent hasher on this machine; asserting against our own
        // would prove only that the code agrees with itself.
        return;
    };

    // The right digest is accepted.
    ext::install(
        &scratch.ext,
        &source,
        &ext::Checks {
            sha256: Some(digest.clone()),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
    )
    .expect("the component matches its own digest");
    ext::remove(&scratch.ext, "tool-fs").expect("removes it again");

    // Uppercase is the same digest — release notes are not consistent about it.
    ext::install(
        &scratch.ext,
        &source,
        &ext::Checks {
            sha256: Some(digest.to_uppercase()),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
    )
    .expect("a digest is hex, and hex is case-insensitive");
    ext::remove(&scratch.ext, "tool-fs").expect("removes it again");

    // Now flip one byte in the middle of the file. Still a component; not the
    // component the digest describes.
    let mut bytes = std::fs::read(&source).expect("reads the component");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&source, &bytes).expect("writes the tampered component");

    let err = ext::install(
        &scratch.ext,
        &source,
        &ext::Checks {
            sha256: Some(digest.clone()),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
    )
    .expect_err("tampered bytes must be refused");
    let ExtError::DigestMismatch {
        expected, actual, ..
    } = &err
    else {
        panic!("a byte-level change is a digest mismatch: {err:?}")
    };
    assert_eq!(expected, &digest, "the refusal names what was asked for");
    assert_ne!(
        actual, &digest,
        "and what the bytes actually are: {actual} vs {digest}"
    );
    assert_eq!(
        names(&scratch.ext),
        Vec::<String>::new(),
        "nothing landed and no staging directory survived"
    );
}

/// A manifest built against another interface package is refused too, and
/// distinguishably — the same gate boot applies, reached through `install`.
#[test]
fn a_manifest_from_another_api_version_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("api");
    let source = offer(&scratch, "tool-fs");

    let manifest = scratch.incoming.join("tool-fs.manifest.toml");
    let text = std::fs::read_to_string(&manifest).expect("reads the manifest");
    std::fs::write(
        &manifest,
        text.replace(
            &format!("api-version = \"{}\"", jan_klod_core::manifest::API_VERSION),
            "api-version = \"9.0.0\"",
        ),
    )
    .expect("rewrites the manifest");

    let checks = resign(&scratch, "tool-fs");
    let err = ext::install(&scratch.ext, &source, &checks).expect_err("must refuse");
    let ExtError::ApiMismatch { theirs, ours, .. } = &err else {
        panic!("an incompatible package version is refused as such: {err:?}")
    };
    assert_eq!(theirs, "9.0.0");
    assert_eq!(ours, jan_klod_core::manifest::API_VERSION);
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

// ---- Signatures (box 4) ----
//
// `common::minisig` builds these fixtures from `ring` + `blake2` because there
// is no `minisign` binary here; see that module for why a hand-built fixture is
// sound. The first test below is what makes the rest meaningful: if a genuinely
// valid signature were *not* accepted, every refusal below would pass for the
// wrong reason.

use crate::common::minisig::Signer;

/// Sign both files of an offer with `signer`.
fn sign_pair(scratch: &Scratch, guest: &str, signer: &Signer) {
    signer.sign(&scratch.incoming.join(format!("{guest}.wasm")));
    signer.sign(&scratch.incoming.join(format!("{guest}.manifest.toml")));
}

fn signed_checks(signer: &Signer) -> ext::Checks {
    ext::Checks {
        sha256: None,
        trusted_keys: vec![signer.public_key_base64()],
        allow_unsigned: false,
    }
}

#[test]
fn a_signature_from_a_trusted_key_over_both_files_installs() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("signed");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    sign_pair(&scratch, "tool-fs", &signer);

    ext::install(&scratch.ext, &source, &signed_checks(&signer))
        .expect("a prehashed signature from a trusted key is accepted");
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair landed, and the .minisig files did not follow them in"
    );
}

/// The case the box exists for: signing the component but **not** the manifest
/// must not be enough. A manifest carries no provenance of its own — it is
/// trusted at boot purely for sitting beside the component — so a signature
/// covering only the `.wasm` would verify the artefact while trusting an
/// attacker's description of what it may ask the host for.
#[test]
fn a_signature_over_the_component_but_not_the_manifest_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("half-signed");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    signer.sign(&source); // the component only

    let err = ext::install(&scratch.ext, &source, &signed_checks(&signer))
        .expect_err("half a signature must not be enough");
    let ExtError::Unsigned { path, .. } = &err else {
        panic!("the manifest is unsigned, and that is what should be reported: {err:?}")
    };
    assert!(
        path.contains("manifest"),
        "the refusal names the file that was not signed: {path}"
    );
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

/// A perfectly valid signature by a key nobody trusted is refused — which is
/// the whole difference between "signed" and "signed by someone we chose".
#[test]
fn a_valid_signature_from_an_untrusted_key_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("untrusted");
    let source = offer(&scratch, "tool-fs");
    let attacker = Signer::new();
    let trusted = Signer::new();
    sign_pair(&scratch, "tool-fs", &attacker);

    let err = ext::install(&scratch.ext, &source, &signed_checks(&trusted))
        .expect_err("a signature by an unnamed key is not trust");
    assert!(
        matches!(err, ExtError::Untrusted { .. }),
        "refused as untrusted rather than as malformed: {err:?}"
    );
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

/// Tampering after signing is caught, and this is the test that would fail if
/// the signature were being checked against the *source* bytes rather than the
/// staged ones.
#[test]
fn bytes_changed_after_signing_are_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("resigned");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    sign_pair(&scratch, "tool-fs", &signer);

    let mut bytes = std::fs::read(&source).expect("reads the component");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&source, &bytes).expect("writes the tampered component");

    let err = ext::install(&scratch.ext, &source, &signed_checks(&signer))
        .expect_err("the signature no longer covers these bytes");
    assert!(
        matches!(err, ExtError::Untrusted { .. }),
        "a signature that does not verify is untrusted: {err:?}"
    );
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

/// The legacy format is refused rather than quietly accepted.
///
/// This is why the fixtures needed `BLAKE2b` at all: `verify(.., allow_legacy)`
/// is passed `false`, so a legacy signature — Ed25519 over the raw file — is
/// `UnexpectedAlgorithm`. Without this test, passing `true` would look
/// identical from the outside.
#[test]
fn a_legacy_format_signature_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("legacy");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    signer.sign_legacy(&source);
    signer.sign_legacy(&scratch.incoming.join("tool-fs.manifest.toml"));

    let err = ext::install(&scratch.ext, &source, &signed_checks(&signer))
        .expect_err("the legacy format is not accepted");
    assert!(
        matches!(err, ExtError::Untrusted { .. }),
        "a legacy signature does not verify under a prehashed-only policy: {err:?}"
    );
}

/// Unsigned installs need the flag *and* a digest, and neither alone will do.
#[test]
fn allow_unsigned_needs_a_digest_and_says_so() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("unsigned");
    let source = offer(&scratch, "tool-fs");

    // No signature, no flag: refused for want of a signature.
    let err = ext::install(&scratch.ext, &source, &ext::Checks::default())
        .expect_err("signed is the default");
    assert!(
        matches!(err, ExtError::Unsigned { .. }),
        "an unsigned component is refused by default: {err:?}"
    );

    // The flag alone: refused, because then nothing at all would vouch for it.
    let err = ext::install(
        &scratch.ext,
        &source,
        &ext::Checks {
            sha256: None,
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
    )
    .expect_err("waiving the signature with no digest leaves no evidence");
    assert!(
        matches!(err, ExtError::DigestRequiredWhenUnsigned),
        "the two flags are paired: {err:?}"
    );

    // Both: permitted.
    let Some(digest) = sha256_of(&source) else {
        return;
    };
    ext::install(
        &scratch.ext,
        &source,
        &ext::Checks {
            sha256: Some(digest),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
    )
    .expect("a digest is evidence enough when the operator says so explicitly");
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"]
    );
}

/// An empty `trusted-keys` list means nothing is trusted, not "skip the check".
#[test]
fn no_configured_keys_refuses_rather_than_accepting_anything() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("no-keys");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    sign_pair(&scratch, "tool-fs", &signer);

    let err = ext::install(&scratch.ext, &source, &ext::Checks::default())
        .expect_err("a signature nobody named is not trust");
    let ExtError::Untrusted { keys, .. } = &err else {
        panic!("refused as untrusted: {err:?}")
    };
    assert_eq!(*keys, 0, "and the message says how many keys were tried");
}
