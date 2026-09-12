//! `ext search` and `ext install <name>` against a registry index.
//!
//! The parsing refusals — a missing field, an unknown `index-version`, a
//! document that is not an index — are unit tests in `core::ext_index`, where
//! they are cheaper. What cannot be tested there is anything needing a *real*
//! component: that a name resolved through an index installs the same bytes a
//! URL would, through the same verification, and that the command an operator
//! actually types prints the capability line the index exists for.
//!
//! **Offline, and signed.** The index is a file on disk and the component is
//! served by a canned `HttpFn`, so nothing here opens a socket. The fixture is
//! signed by a test key rather than installed with `--allow-unsigned`: signed
//! is the installer's default, so waiving it would test a path a real install
//! does not take.

use std::path::Path;

use jan_klod_core::ext_index::{self, IndexError};

use crate::common;
use crate::ext_install::{canned_files, names, published, scratch, Scratch};

/// An index describing one published component, written to a file.
///
/// The digest is the served bytes' own, computed here, because this fixture
/// stands in for a generated index — `registry_index` measures the file it
/// publishes, and a fixture that stated a digest from somewhere else would be
/// testing a broken index rather than a working one. The digest check itself is
/// covered by `ext_install::a_correct_digest_installs_and_a_tampered_byte_is_refused`,
/// which takes its expected value from the system's hasher.
fn write_index(scratch: &Scratch, component: &[u8], signature: &str) -> String {
    use std::fmt::Write;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(component);
    let hex = digest.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    let document = serde_json::json!({
        "index-version": 1,
        "extensions": [{
            "name": "tool-fs",
            "version": "0.1.0",
            "api-version": jan_klod_core::manifest::API_VERSION,
            "kind": "tool",
            "capabilities": ["host-fs", "host-log"],
            "description": "reads and writes inside the workspace",
            "author": "PromptPasture",
            "url": "https://example.com/ext/tool-fs.wasm",
            "sha256": hex,
            "signature": signature,
            "size": component.len(),
        }]
    });
    let path = scratch.incoming.join("index.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&document).expect("serializes"),
    )
    .expect("writes the fixture index");
    path.display().to_string()
}

/// The acceptance: a name resolved through a **local** index installs, with no
/// network at all.
///
/// `registry.url` pointing at a path rather than a URL is not a test-only
/// shortcut — it is the documented behaviour, and it is what an air-gapped
/// operator with a mirrored index uses.
#[test]
fn a_name_from_a_local_index_installs_offline() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("index-install");
    let (files, checks) = published(&scratch, "tool-fs");
    let component = files[0].1.clone();
    let signature = String::from_utf8(files[2].1.clone()).expect("a .minisig is text");
    let source = write_index(&scratch, &component, &signature);
    let (http, asked) = canned_files(files);

    // No `http` is passed for the index itself in any meaningful sense: the
    // path branch never calls it, which the recorded URLs below prove.
    let entries = ext_index::load(&source, &http).expect("a local index reads");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].capabilities,
        ["host-fs", "host-log"],
        "the capability line survives the round trip — it is the point of the format"
    );

    let installed = ext_index::install(&scratch.ext, &entries, "tool-fs", &source, &checks, &http)
        .expect("a listed, signed component installs");
    assert_eq!(installed.name, "tool-fs");
    assert_eq!(
        names(&scratch.ext),
        ["tool-fs.manifest.toml", "tool-fs.wasm"],
        "the pair landed, and the signatures did not follow them in"
    );

    let asked = asked.lock().expect("not poisoned").clone();
    assert!(
        !asked.iter().any(|url| url.ends_with("index.json")),
        "a file:// index is read, not fetched: {asked:?}"
    );
    assert_eq!(
        asked.first().map(String::as_str),
        Some("https://example.com/ext/tool-fs.wasm"),
        "the URL came from the index rather than being guessed"
    );
}

/// The index supplies the digest, and it is really checked.
///
/// Without this the previous test would pass on an installer that ignored
/// `sha256` entirely — the signature alone would carry it. Serving different
/// bytes than the index describes has to be refused as a mismatch.
#[test]
fn bytes_that_are_not_the_ones_the_index_describes_are_refused() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("index-digest");
    let (mut files, checks) = published(&scratch, "tool-fs");
    let honest = files[0].1.clone();
    let signature = String::from_utf8(files[2].1.clone()).expect("a .minisig is text");
    let source = write_index(&scratch, &honest, &signature);

    // The index is written from the honest bytes; the server then answers with
    // a flipped one.
    let middle = files[0].1.len() / 2;
    files[0].1[middle] ^= 0xff;
    let (http, _) = canned_files(files);

    let entries = ext_index::load(&source, &http).expect("the index reads");
    let err = ext_index::install(&scratch.ext, &entries, "tool-fs", &source, &checks, &http)
        .expect_err("bytes the index does not describe must be refused");
    assert!(
        matches!(
            err,
            IndexError::Install(jan_klod_core::ext::ExtError::DigestMismatch { .. })
        ),
        "the index's digest is checked, not just carried: {err:?}"
    );
    assert_eq!(names(&scratch.ext), [] as [String; 0], "and nothing landed");
}

/// A digest on the command line and a digest in the index are two claims about
/// one set of bytes. Disagreement stops the install rather than being resolved
/// by precedence.
#[test]
fn a_digest_the_index_contradicts_is_refused_before_anything_is_fetched() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("index-disagree");
    let (files, mut checks) = published(&scratch, "tool-fs");
    let component = files[0].1.clone();
    let source = write_index(&scratch, &component, "");
    let (http, asked) = canned_files(files);

    checks.sha256 =
        Some("0000000000000000000000000000000000000000000000000000000000000000".to_owned());
    let entries = ext_index::load(&source, &http).expect("the index reads");
    let err = ext_index::install(&scratch.ext, &entries, "tool-fs", &source, &checks, &http)
        .expect_err("two digests that disagree must stop the install");
    assert!(matches!(err, IndexError::DigestDisagrees { .. }), "{err:?}");
    assert!(
        asked.lock().expect("not poisoned").is_empty(),
        "and nothing was fetched, because neither digest is trustworthy yet"
    );
}

/// An unreachable index is a refusal naming the URL, not an empty result.
///
/// The distinction matters: an index that answers 404 and an index with nothing
/// in it would otherwise both print "no results", and only one of them is
/// something the operator can fix.
#[test]
fn an_unreachable_index_says_so_rather_than_reading_as_empty() {
    let (http, _) = canned_files(vec![("nothing-served", Vec::new())]);
    let err = ext_index::load("https://example.com/index.json", &http)
        .expect_err("a 404 is not an empty registry");
    let IndexError::Fetch { at, detail } = &err else {
        panic!("an unreachable index is a fetch failure: {err:?}")
    };
    assert_eq!(at, "https://example.com/index.json");
    assert!(
        detail.contains("404"),
        "and says what the server said: {detail}"
    );
}

/// A registry URL the egress policy refuses is refused **before a byte moves**,
/// exactly as a component URL is. An index is a list of places to download
/// executable code from, so it is not a lesser destination than the code.
#[test]
fn a_loopback_index_is_refused_before_it_is_requested() {
    let (http, asked) = canned_files(vec![("index.json", b"{}".to_vec())]);
    let err = ext_index::load("http://127.0.0.1:8080/index.json", &http)
        .expect_err("a loopback registry is not a public address");
    assert!(
        matches!(
            err,
            IndexError::Install(jan_klod_core::ext::ExtError::RefusedByPolicy { .. })
        ),
        "{err:?}"
    );
    assert!(
        asked.lock().expect("not poisoned").is_empty(),
        "refused before the request, not after the answer"
    );
}

/// A malformed index names the file and what was wrong with it.
#[test]
fn a_malformed_local_index_names_the_file_and_the_problem() {
    let scratch = scratch("index-malformed");
    let path = scratch.incoming.join("index.json");
    std::fs::write(&path, "{ this is not json").expect("writes a broken index");
    let (http, _) = canned_files(Vec::new());

    let source = path.display().to_string();
    let err = ext_index::load(&source, &http).expect_err("a broken index must be refused");
    let rendered = err.to_string();
    assert!(
        rendered.contains(&source),
        "the refusal names the file the operator has to fix: {rendered}"
    );

    let missing = scratch.incoming.join("nowhere.json").display().to_string();
    let err = ext_index::load(&missing, &http).expect_err("an absent index must be refused");
    assert!(
        matches!(err, IndexError::Unreadable { .. }),
        "an absent index is unreadable, not empty: {err:?}"
    );
}

// ---- The command an operator actually types ----

/// Run `jan-klod-gateway ext <args…>` with `cwd` as its working directory.
///
/// The real binary, because the acceptance is about what `ext search` *prints*
/// and that rendering lives in the gateway rather than in the core. A test that
/// called the library would have proved the entries parse and nothing about the
/// capability line reaching a terminal.
fn gateway(cwd: &Path, args: &[&str]) -> (bool, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_jan-klod-gateway"))
        .arg("ext")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("the gateway runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A `config.yaml` naming nothing but the registry.
///
/// `Checks::from_config_path` and `url_from_config_path` both read one
/// top-level key rather than resolving the whole document, which is what lets
/// this be four lines instead of the shipped config — and what stops `ext
/// search` demanding a provider's API key.
fn config_naming(scratch: &Scratch, url: &str) -> std::path::PathBuf {
    let path = scratch.incoming.join("config.yaml");
    std::fs::write(
        &path,
        format!("registry:\n  trusted-keys: []\n  url: {url}\n"),
    )
    .expect("writes a config");
    path
}

/// The acceptance: `ext search` prints what each hit asks the host for.
#[test]
fn ext_search_prints_the_capabilities_of_every_hit() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let scratch = scratch("index-cli-search");
    let (files, _) = published(&scratch, "tool-fs");
    let source = write_index(&scratch, &files[0].1, "");
    config_naming(&scratch, &source);

    let (ok, out, err) = gateway(&scratch.incoming, &["search", "fs"]);
    assert!(ok, "ext search failed: {err}");
    assert!(out.contains("tool-fs"), "the hit is named: {out}");
    assert!(
        out.contains("may use host-fs, host-log"),
        "the capability line is the point of the command: {out}"
    );
    assert!(
        out.contains("unsigned"),
        "an entry nothing vouches for says so: {out}"
    );

    // And `list --remote` is the same view with nothing filtered out.
    let (ok, listed, err) = gateway(&scratch.incoming, &["list", "--remote"]);
    assert!(ok, "ext list --remote failed: {err}");
    assert_eq!(listed, out, "one traversal, one rendering");

    // A term that matches nothing is a success with an explanation, not a
    // failure: the registry worked, the search did not find anything.
    let (ok, empty, _) = gateway(&scratch.incoming, &["search", "nothing-like-this"]);
    assert!(ok, "a search with no hits is not a failure");
    assert!(
        empty.contains("1 entries"),
        "it says how big the index was, so 'no hits' cannot be confused with \
         'the registry is empty': {empty}"
    );
}

/// A broken registry has to fail the command with something actionable — not a
/// panic, and not silence.
#[test]
fn a_broken_registry_fails_the_command_with_a_message() {
    let scratch = scratch("index-cli-broken");
    let broken = scratch.incoming.join("index.json");
    std::fs::write(&broken, "{ not json").expect("writes a broken index");
    config_naming(&scratch, &broken.display().to_string());

    let (ok, out, err) = gateway(&scratch.incoming, &["search", "fs"]);
    assert!(!ok, "a registry that cannot be read is a failed command");
    assert!(out.is_empty(), "and prints no results: {out}");
    assert!(
        err.contains("cannot be read"),
        "the refusal reaches the operator: {err}"
    );
    assert!(
        !err.contains("panicked"),
        "a malformed index is a refusal, never a panic: {err}"
    );

    // And a config that names no registry at all says which key is missing,
    // rather than searching nothing.
    std::fs::write(
        scratch.incoming.join("config.yaml"),
        "registry:\n  trusted-keys: []\n",
    )
    .expect("writes a config naming no registry");
    let (ok, _, err) = gateway(&scratch.incoming, &["search", "fs"]);
    assert!(!ok);
    assert!(
        err.contains("registry.url"),
        "it names the key to add: {err}"
    );
}
