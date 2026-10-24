//! `ext install` against real components: valid ones land and load, and a
//! manifest that disagrees with the real imports is refused. Refusals that need
//! only bytes are unit tests in `core::ext`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jk_wasm::ext::{self, Declaration, ExtError};

use crate::common;

/// Scratch `ext/` and sources dir, removed on drop.
pub struct Scratch {
    root: PathBuf,
    pub ext: PathBuf,
    pub incoming: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn scratch(tag: &str) -> Scratch {
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

/// Copies a staged guest and its manifest into `incoming`, returning the component path.
pub fn offer(scratch: &Scratch, guest: &str) -> PathBuf {
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

/// SHA-256 of a file, from a system tool that shares no code with the host, so a
/// wrong hex encoder cannot agree with itself. `None` when no tool exists; the caller skips.
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
        return text.split_whitespace().next().map(str::to_ascii_lowercase);
    }
    None
}

/// A properly signed offer and the checks that accept it. Structural tests need a
/// signature, or they are refused before reaching the check they cover.
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

/// Re-signs an offer whose files were edited after signing, so the structural
/// check is what refuses it rather than a stale signature.
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
pub fn names(dir: &Path) -> Vec<String> {
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

    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair lands together and the staging directory is gone"
    );

    let Declaration::Present(manifest) = &installed.declaration else {
        panic!("a component that installed has a readable manifest: {installed:?}")
    };
    assert_eq!(manifest.capabilities, ["host-fs"]);

    let listed = ext::list(&scratch.ext).expect("lists");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].declaration, installed.declaration);

    let engine = wasmtime::Engine::default();
    wasmtime::component::Component::from_file(&engine, scratch.ext.join("tool-fs.wasm"))
        .expect("what install accepted, the runtime can load");
}

/// A manifest that under-declares the component's imports is refused and nothing lands.
/// Operators read the declaration before granting anything.
#[test]
fn a_manifest_that_under_declares_is_refused_and_nothing_lands() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("under-declared");
    let source = offer(&scratch, "tool-fs");

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

/// A stem naming no category is refused before any byte moves (#222). Left to
/// adoption, the component would land, report success, and be uncallable later.
/// The valid-install test above is the control: the same bytes under a `tool-` stem.
#[test]
fn a_component_whose_stem_names_no_category_is_refused_and_nothing_lands() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("uncategorised");
    let signer = Signer::new();
    let staged = common::repo_root().join("ext");
    let source = scratch.incoming.join("self-built.wasm");
    std::fs::copy(staged.join("tool-fs.wasm"), &source).expect("copies the component");
    std::fs::copy(
        staged.join("tool-fs.manifest.toml"),
        scratch.incoming.join("self-built.manifest.toml"),
    )
    .expect("copies the manifest");
    sign_pair(&scratch, "self-built", &signer);

    let err = ext::install(&scratch.ext, &source, &signed_checks(&signer))
        .expect_err("`self` is not a category");
    let ExtError::UncategorisedStem { reason } = &err else {
        panic!("an uncategorised stem is refused as such: {err:?}")
    };
    for category in jk_wasm::CATEGORIES {
        assert!(
            reason.contains(category),
            "the refusal must name `{category}`: {reason}"
        );
    }
    assert_eq!(
        names(&scratch.ext),
        Vec::<String>::new(),
        "an uncallable component reached ext/ anyway"
    );
}

/// The digest check: the right digest installs, and a flipped byte is refused with
/// both digests named. Flipping a byte keeps the file valid wasm, so only the digest can catch it.
#[test]
fn a_correct_digest_installs_and_a_tampered_byte_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("digest");
    let source = offer(&scratch, "tool-fs");
    let Some(digest) = sha256_of(&source) else {
        return;
    };

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

/// A manifest built for another API version is refused, the same gate boot applies.
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
            &format!("api-version = \"{}\"", jk_wasm::manifest::API_VERSION),
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
    assert_eq!(ours, jk_wasm::manifest::API_VERSION);
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

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

/// Signing only the component is not enough: the manifest has no provenance of
/// its own, so an unsigned manifest would let an attacker's declaration through.
#[test]
fn a_signature_over_the_component_but_not_the_manifest_is_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("half-signed");
    let source = offer(&scratch, "tool-fs");
    let signer = Signer::new();
    signer.sign(&source);

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

/// Bytes changed after signing must fail. This also catches a signature checked against the source rather than the staged bytes.
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

/// The legacy signature format is refused. `verify` runs with `allow_legacy` false,
/// and without this test passing `true` would look identical from outside.
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

/// Unsigned installs need flag *and* digest; neither alone suffices.
#[test]
fn allow_unsigned_needs_a_digest_and_says_so() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("unsigned");
    let source = offer(&scratch, "tool-fs");

    let err = ext::install(&scratch.ext, &source, &ext::Checks::default())
        .expect_err("signed is the default");
    assert!(
        matches!(err, ExtError::Unsigned { .. }),
        "an unsigned component is refused by default: {err:?}"
    );

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

/// Acceptance line 2: what `install` accepts, a real boot loads. Compiling (as the
/// valid-install test does) is not booting; boot also resolves the instance, reads
/// the manifest, and cross-checks imports.
#[test]
fn a_component_installed_here_is_loaded_by_a_real_boot() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("boots");
    let (source, checks) = signed_offer(&scratch, "tool-fs");
    ext::install(&scratch.ext, &source, &checks).expect("installs");

    let config = scratch.root.join("config.yaml");
    std::fs::write(
        &config,
        "
execution:
  enabled: false
extensions:
  tool:
    fs:
      enabled: true
",
    )
    .expect("writes a config");

    let runtime = jk_agent::Runtime::boot(&config, &scratch.ext)
        .expect("the core boots against what install put there");
    let loaded = runtime
        .extensions()
        .iter()
        .find(|e| e.instance.id == "tool.fs")
        .expect("the instance resolved");
    assert!(
        matches!(loaded.state, jk_wasm::LoadState::Compiled(_)),
        "compiled rather than missing — the manifest beside it satisfied the \
         boot-time cross-check too"
    );
    assert_eq!(
        loaded.capabilities.as_deref(),
        Some(["host-fs".to_owned()].as_slice()),
        "and boot read the same capabilities install reported"
    );
}

/// A loopback URL is refused before anything goes on the wire. Only the server's
/// hit count can prove that, so this uses `common::Countable`.
#[test]
fn a_loopback_source_is_refused_before_anything_is_fetched() {
    let sentinel = common::Countable::start();
    let url = format!("http://127.0.0.1:{}/tool-thing.wasm", sentinel.port);

    let err = ext::check_remote(&url).expect_err("loopback is not a public destination");
    let ExtError::RefusedByPolicy { url: named, .. } = &err else {
        panic!("refused by the policy, not something else: {err:?}")
    };
    assert_eq!(named, &url, "the refusal names the URL the operator gave");
    assert!(
        err.to_string().contains("egress policy"),
        "and names the policy, so the reason is findable: {err}"
    );

    sentinel.only_hit();
}

/// Address classes the policy exists for. None can host a sentinel, so these assert the decision alone.
#[test]
fn private_link_local_and_metadata_sources_are_refused() {
    for url in [
        "http://10.0.0.1/x.wasm",
        "http://192.168.1.10/x.wasm",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/x.wasm",
        "http://localhost/x.wasm",
    ] {
        let err = ext::check_remote(url).expect_err("not a public destination");
        assert!(
            matches!(err, ExtError::RefusedByPolicy { .. }),
            "{url} is refused by the policy: {err:?}"
        );
    }
}

/// Only http(s) is remote. `file:///etc/passwd` is a local path, not a fetch the policy has no opinion about.
#[test]
fn only_http_and_https_count_as_remote() {
    for remote in [
        "http://example.com/x.wasm",
        "https://example.com/x.wasm",
        "HTTPS://EXAMPLE.COM/x.wasm",
    ] {
        assert!(ext::looks_remote(remote), "{remote} is remote");
    }
    for local in [
        "./tool-thing.wasm",
        "/abs/tool-thing.wasm",
        "tool-thing.wasm",
        "file:///etc/passwd",
        "ftp://example.com/x.wasm",
        "httpx://example.com/x.wasm",
    ] {
        assert!(!ext::looks_remote(local), "{local} is not remote");
    }
}

/// Serves a set of files by URL and records each URL requested. Uses public
/// `example.com` URLs, which the policy permits without a loopback grant.
pub fn canned_files(
    files: Vec<(&'static str, Vec<u8>)>,
) -> (jk_wasm::route::HttpFn, Arc<std::sync::Mutex<Vec<String>>>) {
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&asked);
    let http: jk_wasm::route::HttpFn = Box::new(move |_m, url, _h, _b, _t| {
        recorded.lock().expect("not poisoned").push(url.to_owned());
        let found = files
            .iter()
            .find(|(name, _)| url.ends_with(name))
            .map(|(_, body)| body.clone());
        Ok(jk_wasm::http::WireResponse {
            status: if found.is_some() { 200 } else { 404 },
            headers: vec![],
            body: found.unwrap_or_default(),
        })
    });
    (http, asked)
}

/// Four files for signed remote install, from staged guest.
pub fn published(scratch: &Scratch, guest: &str) -> (Vec<(&'static str, Vec<u8>)>, ext::Checks) {
    let component = offer(scratch, guest);
    let signer = crate::common::minisig::Signer::new();
    signer.sign(&component);
    let manifest = scratch.incoming.join(format!("{guest}.manifest.toml"));
    signer.sign(&manifest);
    let read = |p: &Path| std::fs::read(p).expect("reads a published file");
    let mut path = component.as_os_str().to_os_string();
    path.push(".minisig");
    let component_sig = read(Path::new(&path));
    let mut path = manifest.as_os_str().to_os_string();
    path.push(".minisig");
    let manifest_sig = read(Path::new(&path));
    (
        vec![
            ("tool-fs.wasm", read(&component)),
            ("tool-fs.manifest.toml", read(&manifest)),
            ("tool-fs.wasm.minisig", component_sig),
            ("tool-fs.manifest.toml.minisig", manifest_sig),
        ],
        ext::Checks {
            sha256: None,
            trusted_keys: vec![signer.public_key_base64()],
            allow_unsigned: false,
        },
    )
}

/// Signed remote install fetches four files, same verification as local.
#[test]
fn a_remote_install_fetches_the_pair_and_both_signatures() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("remote");
    let (files, checks) = published(&scratch, "tool-fs");
    let (http, asked) = canned_files(files);

    let installed = ext::install_from_url(
        &scratch.ext,
        "https://example.com/ext/tool-fs.wasm",
        &checks,
        &http,
    )
    .expect("a signed remote install lands");
    assert_eq!(installed.name, "tool-fs");
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair landed and the signatures did not follow them in"
    );

    let asked = asked.lock().expect("not poisoned").clone();
    assert_eq!(
        asked,
        [
            "https://example.com/ext/tool-fs.wasm",
            "https://example.com/ext/tool-fs.manifest.toml",
            "https://example.com/ext/tool-fs.wasm.minisig",
            "https://example.com/ext/tool-fs.manifest.toml.minisig",
        ],
        "siblings are resolved against the component's URL, not guessed"
    );
}

/// Bytes tampered in flight are refused like a tampered local file: downloads have no weaker path.
#[test]
fn bytes_tampered_in_flight_are_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("remote-tamper");
    let (mut files, checks) = published(&scratch, "tool-fs");
    let middle = files[0].1.len() / 2;
    files[0].1[middle] ^= 0xff;
    let (http, _) = canned_files(files);

    let err = ext::install_from_url(
        &scratch.ext,
        "https://example.com/ext/tool-fs.wasm",
        &checks,
        &http,
    )
    .expect_err("the signature no longer covers these bytes");
    assert!(
        matches!(err, ExtError::Untrusted { .. }),
        "refused by the same check a local tampered file hits: {err:?}"
    );
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");
}

/// A waived signature fetches only the two files; a 404 on `.minisig` must not fail an unsigned install.
#[test]
fn an_unsigned_remote_install_does_not_fetch_signatures() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("remote-unsigned");
    let (files, _) = published(&scratch, "tool-fs");
    let two = vec![files[0].clone(), files[1].clone()];
    let (http, asked) = canned_files(two);

    let Some(digest) = sha256_of(&scratch.incoming.join("tool-fs.wasm")) else {
        return;
    };
    ext::install_from_url(
        &scratch.ext,
        "https://example.com/ext/tool-fs.wasm",
        &ext::Checks {
            sha256: Some(digest),
            trusted_keys: Vec::new(),
            allow_unsigned: true,
        },
        &http,
    )
    .expect("an unsigned remote install with a digest lands");

    let asked = asked.lock().expect("not poisoned").clone();
    assert_eq!(asked.len(), 2, "no signature was requested: {asked:?}");
}

/// URL with query refused, not mangled silently.
#[test]
fn a_url_with_a_query_is_refused_rather_than_mangled() {
    let (http, asked) = canned_files(vec![]);
    let err = ext::install_from_url(
        Path::new("/nonexistent"),
        "https://example.com/ext/tool-fs.wasm?token=abc",
        &ext::Checks::default(),
        &http,
    )
    .expect_err("a query cannot survive sibling derivation");
    assert!(
        matches!(err, ExtError::OpaqueUrl { .. }),
        "refused for its shape: {err:?}"
    );
    assert!(
        asked.lock().expect("not poisoned").is_empty(),
        "and nothing was requested"
    );
}

/// `install_from_url` refuses a denied destination before asking the client. The other tests use permitted URLs, so this is what catches the check being dropped.
#[test]
fn a_refused_destination_is_not_even_requested() {
    let (http, asked) = canned_files(vec![]);
    let err = ext::install_from_url(
        Path::new("/nonexistent"),
        "http://127.0.0.1:9/ext/tool-fs.wasm",
        &ext::Checks::default(),
        &http,
    )
    .expect_err("loopback is not a public destination");
    assert!(
        matches!(err, ExtError::RefusedByPolicy { .. }),
        "refused by the policy: {err:?}"
    );
    assert!(
        asked.lock().expect("not poisoned").is_empty(),
        "and the client was never asked: {:?}",
        asked.lock().expect("not poisoned")
    );
}

/// A redirect from a permitted origin to a refused one ends the install and the destination is never touched.
/// The installer gets a public URL; the client reaches a loopback redirector that the policy permits, but not the sentinel behind it.
#[test]
fn a_redirect_to_a_refused_destination_ends_a_remote_install() {
    let scratch = scratch("remote-redirect");
    let forbidden = common::Countable::start();
    let (redirect_port, _) = crate::egress_boundary::redirector_to(format!(
        "http://127.0.0.1:{}/tool-fs.wasm",
        forbidden.port
    ));

    let entry = format!("http://127.0.0.1:{redirect_port}/tool-fs.wasm");
    let policy = jk_wasm::egress::EgressPolicy::public_only()
        .allowing(&format!("http://127.0.0.1:{redirect_port}"));
    let http: jk_wasm::route::HttpFn = Box::new(move |method, _asked, headers, body, timeout| {
        jk_wasm::http::fetch_within(&policy, method, &entry, headers, body, timeout)
    });

    let err = ext::install_from_url(
        &scratch.ext,
        "https://example.com/ext/tool-fs.wasm",
        &ext::Checks::default(),
        &http,
    )
    .expect_err("the redirect leaves the permitted origin");
    assert!(
        matches!(err, ExtError::Fetch { .. }),
        "the install ends on the fetch, not on a later check: {err:?}"
    );
    assert_eq!(names(&scratch.ext), Vec::<String>::new(), "nothing landed");

    forbidden.only_hit();
}
