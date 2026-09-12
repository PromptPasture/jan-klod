//! Generate `index.json` — what a registry says about a directory of staged
//! components, before anyone downloads one.
//!
//! Usage: `registry_index <ext-dir> <base-url> <author> <out-file>`, normally
//! through `scripts/registry-index.sh` / `make registry-index`.
//!
//! # Why this is a `jan-klod-core` example rather than a shell script
//!
//! Six of the eleven fields per entry are *the manifest's own*, and this reads
//! them with [`jan_klod_core::ext::list`] — the same reader `ext list` and the
//! boot path use. A shell generator would be a second parser of
//! `ext/*.manifest.toml`, free to disagree with the first about what a
//! component declares, and the whole value of an index is that the capability
//! line in it is the one the host will enforce.
//!
//! # What each field is, and where it comes from
//!
//! `name`, `version`, `api-version`, `kind`, `capabilities`, `description` are
//! read from the manifest. `sha256` and `size` are measured from the `.wasm`
//! beside it. `url` is derived from the base URL. `author` is the publisher of
//! this index, passed in, because a manifest names no author and inventing a
//! per-component one from an empty `Cargo.toml` field would be decoration.
//!
//! **`sha256` and `signature` describe the individual `.wasm` and its
//! `.minisig`, never an archive.** `ext install` verifies per *file* and
//! derives the manifest and signature URLs as siblings of the component's, so
//! an index whose digest covered a tarball would describe something the
//! installer never fetches.
//!
//! # Determinism
//!
//! Two runs over one directory must produce byte-identical output, which
//! `scripts/registry-index-drift.sh` checks and probes. Nothing here reads the
//! clock, the environment, or the filesystem's own ordering: entries come out
//! sorted by name (`ext::list` sorts), capabilities sorted (`Manifest::beside`
//! sorts), and the JSON key order is this file's field order because
//! `serde_json` writes a struct in declaration order.

use std::path::Path;

use jan_klod_core::ext::{list, Declaration};

/// The index format's own version, so a reader can refuse one it does not
/// understand rather than guessing at missing keys.
const INDEX_VERSION: u32 = 1;

/// One published component, as a registry describes it.
///
/// Field order is the serialized key order — see the module note on
/// determinism.
#[derive(serde::Serialize)]
struct Entry {
    name: String,
    version: String,
    #[serde(rename = "api-version")]
    api_version: String,
    kind: String,
    capabilities: Vec<String>,
    description: String,
    author: String,
    url: String,
    sha256: String,
    signature: String,
    size: u64,
}

/// The whole document.
#[derive(serde::Serialize)]
struct Index {
    #[serde(rename = "index-version")]
    index_version: u32,
    extensions: Vec<Entry>,
}

fn fail(message: &str) -> ! {
    eprintln!("registry-index: {message}");
    std::process::exit(1)
}

/// A SHA-256 digest as lowercase hex — the spelling `ext install --sha256`
/// compares against.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Where the component will be served from.
///
/// The base is refused if it carries a query or a fragment, for the reason
/// `ext::ExtError::OpaqueUrl` gives: the manifest and signature URLs are
/// derived as siblings by relative resolution, which drops a query — so an
/// index built on such a base would name component URLs whose companions
/// 404. Better to refuse while generating than to publish it.
fn component_url(base: &str, name: &str) -> String {
    let parsed =
        url::Url::parse(base).unwrap_or_else(|err| fail(&format!("{base} is not a URL: {err}")));
    if parsed.query().is_some() || parsed.fragment().is_some() {
        fail(&format!(
            "{base} carries a query or fragment. `ext install` derives the manifest and \
             signature URLs as siblings of the component's, and a relative reference drops \
             a query, so those companions would be requested without it."
        ));
    }
    format!("{}/{name}.wasm", base.trim_end_matches('/'))
}

/// The detached signature over the component, when there is one.
///
/// Empty until something signs the first-party components — that is
/// [#93](https://github.com/PromptPasture/jan-klod/issues/93), deferred until
/// before the first release. Emitting `""` rather than omitting the key says
/// "nothing vouches for these bytes yet", which is the true statement; a
/// reader that requires provenance can then refuse the entry instead of
/// finding no key and assuming the index is an older shape.
///
/// The whole `.minisig` text, not just its base64 line: that is what
/// `minisign_verify::Signature::decode` takes, so a reader can verify from the
/// index without a second fetch.
fn signature_beside(component: &Path) -> String {
    let mut name = component.as_os_str().to_os_string();
    name.push(".minisig");
    std::fs::read_to_string(Path::new(&name)).unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [ext_dir, base_url, author, out] = args.as_slice() else {
        fail("usage: registry_index <ext-dir> <base-url> <author> <out-file>")
    };

    let staged = list(Path::new(ext_dir)).unwrap_or_else(|err| fail(&format!("{err}")));

    let mut extensions = Vec::with_capacity(staged.len());
    for component in staged {
        // A component whose manifest is absent or broken is not described
        // rather than described vaguely: every field below the name comes from
        // that manifest, and an entry carrying guesses is worse than an index
        // that is short by one. It is also a refusal with a fix — `make ext`
        // regenerates the manifest — so failing here is the message.
        let manifest = match component.declaration {
            Declaration::Present(manifest) => manifest,
            Declaration::Absent => fail(&format!(
                "{} has no manifest beside it; run `make ext`",
                component.component.display()
            )),
            Declaration::Broken(reason) => fail(&format!(
                "{}'s manifest cannot be read: {reason}",
                component.component.display()
            )),
        };
        let bytes = std::fs::read(&component.component)
            .unwrap_or_else(|err| fail(&format!("{}: {err}", component.component.display())));

        extensions.push(Entry {
            url: component_url(base_url, &manifest.name),
            sha256: hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes)),
            signature: signature_beside(&component.component),
            // `try_from` rather than `as`: a size that did not convert would
            // be a wrong number in a published index, and there is no sensible
            // wrong number for "how many bytes will you download".
            size: u64::try_from(bytes.len())
                .unwrap_or_else(|err| fail(&format!("{} is unmeasurable: {err}", manifest.name))),
            name: manifest.name,
            version: manifest.version,
            api_version: manifest.api_version,
            kind: manifest.kind,
            capabilities: manifest.capabilities,
            description: manifest.description,
            author: author.clone(),
        });
    }

    let index = Index {
        index_version: INDEX_VERSION,
        extensions,
    };
    let mut json = serde_json::to_string_pretty(&index)
        .unwrap_or_else(|err| fail(&format!("serializing the index: {err}")));
    // A trailing newline, so the file is a text file and a diff of two of them
    // reads as lines rather than as one changed line.
    json.push('\n');

    let path = Path::new(out);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|err| fail(&format!("{}: {err}", parent.display())));
        }
    }
    // Written once, at the end: a generator that refuses half way through must
    // not leave a truncated index behind for the next reader to believe.
    std::fs::write(path, &json).unwrap_or_else(|err| fail(&format!("{out}: {err}")));

    println!(
        "registry-index: {} extension(s) -> {out}",
        index.extensions.len()
    );
}
