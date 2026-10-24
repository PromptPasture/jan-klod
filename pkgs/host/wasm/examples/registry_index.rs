//! Generate `index.json`: what a registry says about a directory of staged components.
//!
//! Usage: `registry_index <ext-dir> <base-url> <author> <out-file>`, normally via `make registry-index`.
//! Entries are read with [`jk_wasm::ext::list`] so the capability line is the one the host enforces;
//! `sha256` and `signature` describe the individual `.wasm`/`.minisig`, never an archive. Output is
//! deterministic: no clock, env or fs ordering, and JSON key order is struct field order.

use std::path::Path;

use jk_wasm::ext::{list, Declaration};

/// The index format's own version, so a reader can refuse one it doesn't
/// understand rather than guessing at missing keys.
const INDEX_VERSION: u32 = 1;

/// One published component, as a registry describes it.
///
/// Field order is the serialized key order.
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

/// A SHA-256 digest as lowercase hex, as `ext install --sha256` spells it.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Where the component will be served from.
///
/// A base with a query or fragment is refused (`ext::ExtError::OpaqueUrl`): relative resolution drops
/// queries, so companions would 404.
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
/// Empty until first-party components are signed (#93); `""` says nothing vouches for the bytes yet.
/// Holds the whole `.minisig` text, which `minisign_verify::Signature::decode` takes.
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
        // Refuse a component whose manifest is absent or broken: every field comes from it, and a guessed
        // entry is worse than a short index. `make ext` regenerates the manifest.
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
            // `try_from` not `as`: a wrapped size would be a silently wrong download size.
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
    // Trailing newline so diffs read as lines.
    json.push('\n');

    let path = Path::new(out);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|err| fail(&format!("{}: {err}", parent.display())));
        }
    }
    // Written once at the end so a refusal halfway leaves no truncated index.
    std::fs::write(path, &json).unwrap_or_else(|err| fail(&format!("{out}: {err}")));

    println!(
        "registry-index: {} extension(s) -> {out}",
        index.extensions.len()
    );
}
