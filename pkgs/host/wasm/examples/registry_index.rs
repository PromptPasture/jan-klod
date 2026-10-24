//! Generate `index.json`: what a registry says about a directory of staged components.

use std::path::Path;

use jk_wasm::ext::{list, Declaration};

const INDEX_VERSION: u32 = 1;

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

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// A base with a query or fragment is refused: relative resolution drops queries, so companions would 404.
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

/// Empty until first-party components are signed; `""` says nothing vouches for the bytes.
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
    json.push('\n');

    let path = Path::new(out);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|err| fail(&format!("{}: {err}", parent.display())));
        }
    }
    std::fs::write(path, &json).unwrap_or_else(|err| fail(&format!("{out}: {err}")));

    println!(
        "registry-index: {} extension(s) -> {out}",
        index.extensions.len()
    );
}
