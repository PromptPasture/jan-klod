//! `ext search` and `ext install <name>` against a registry index. Offline and
//! signed: the index is a file and the component comes from a canned `HttpFn`.
//! Parsing refusals are unit tests in `core::ext_index`.

use std::path::Path;

use jk_wasm::ext_index::{self, IndexError};

use crate::common;
use crate::ext_install::{canned_files, names, published, scratch, Scratch};

/// An index describing one published component, written to a file. The digest is
/// computed from the served bytes, standing in for a generated index; the digest
/// check itself is covered in `ext_install`.
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
            "api-version": jk_wasm::manifest::API_VERSION,
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

/// A name resolved through a **local** index installs with no network. A path as
/// `registry.url` is documented behaviour for air-gapped mirrors, not a test shortcut.
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

/// The index's digest is really checked: bytes it does not describe are refused as a mismatch.
/// Otherwise the previous test would pass for an installer that ignored `sha256`.
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

    let middle = files[0].1.len() / 2;
    files[0].1[middle] ^= 0xff;
    let (http, _) = canned_files(files);

    let entries = ext_index::load(&source, &http).expect("the index reads");
    let err = ext_index::install(&scratch.ext, &entries, "tool-fs", &source, &checks, &http)
        .expect_err("bytes the index does not describe must be refused");
    assert!(
        matches!(
            err,
            IndexError::Install(jk_wasm::ext::ExtError::DigestMismatch { .. })
        ),
        "the index's digest is checked, not just carried: {err:?}"
    );
    assert_eq!(names(&scratch.ext), [] as [String; 0], "and nothing landed");
}

/// A command-line digest that contradicts the index's stops the install; they are not resolved by precedence.
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

/// An unreachable index is a refusal naming the URL, not an empty result: a 404 must not read as "no results".
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

/// A registry URL the egress policy refuses is refused before a byte moves, as a
/// component URL is: an index lists executable code to download.
#[test]
fn a_loopback_index_is_refused_before_it_is_requested() {
    let (http, asked) = canned_files(vec![("index.json", b"{}".to_vec())]);
    let err = ext_index::load("http://127.0.0.1:8080/index.json", &http)
        .expect_err("a loopback registry is not a public address");
    assert!(
        matches!(
            err,
            IndexError::Install(jk_wasm::ext::ExtError::RefusedByPolicy { .. })
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

/// Runs `jan-klod-gateway ext <args…>` in `cwd`. Uses the real binary because the
/// capability line is rendered by the gateway, not the core.
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

/// A `config.yaml` naming only the registry. The readers take one top-level key, so `ext search` does not demand a provider's API key.
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

    let (ok, listed, err) = gateway(&scratch.incoming, &["list", "--remote"]);
    assert!(ok, "ext list --remote failed: {err}");
    assert_eq!(listed, out, "one traversal, one rendering");

    let (ok, empty, _) = gateway(&scratch.incoming, &["search", "nothing-like-this"]);
    assert!(ok, "a search with no hits is not a failure");
    assert!(
        empty.contains("1 entries"),
        "it says how big the index was, so 'no hits' cannot be confused with \
         'the registry is empty': {empty}"
    );
}

/// A broken registry fails the command with a message: no panic, no silence.
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
