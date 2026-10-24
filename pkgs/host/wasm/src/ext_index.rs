//! The registry index `ext search` reads and `ext install <name>` resolves through; not trusted for install decisions.

use std::path::Path;

use crate::ext::{self, Checks, ExtError, Installed};
use crate::route::HttpFn;

/// The index format this build understands; any other is refused.
pub const INDEX_VERSION: u64 = 1;

/// One published component, as the index describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Component name, and the name `ext install <name>` takes.
    pub name: String,
    /// The component's own version.
    pub version: String,
    /// The `jan-klod:interfaces` package version it was built against.
    pub api_version: String,
    /// Category (`provider`, `tool`, `interceptor`, …).
    pub kind: String,
    /// The `host-*` interfaces it declares.
    pub capabilities: Vec<String>,
    /// What it is for; empty when absent.
    pub description: String,
    /// Who published it; empty when absent.
    pub author: String,
    /// Where the `.wasm` is served from; manifest and signature are siblings.
    pub url: String,
    /// Expected SHA-256 of the component, as hex.
    pub sha256: String,
    /// Detached minisign signature; empty means nothing vouches for the bytes.
    pub signature: String,
    /// Size in bytes, or `0` when unstated.
    pub size: u64,
}

/// Why an index could not be used.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// `registry.url` is absent, so there's no index to read.
    #[error(
        "no `registry.url` in {path}. `ext search` reads the index of published \
         components from there; name one, or install from a path or URL instead"
    )]
    NoUrl {
        /// The config that does not name one.
        path: String,
    },
    /// `registry.url` is present and is not a string.
    #[error("`registry.url` must be the URL or path of an index.json")]
    UrlNotAString,
    /// `config.yaml` could not be read or parsed.
    #[error("reading {path}: {detail}")]
    Config {
        /// The config file that could not be used.
        path: String,
        /// What the config crate objected to.
        detail: String,
    },
    /// A local index file could not be read.
    #[error("reading the index at {at}")]
    Unreadable {
        /// Where the index was looked for.
        at: String,
        /// The underlying I/O error.
        #[source]
        cause: std::io::Error,
    },
    /// A remote index could not be fetched.
    #[error("fetching the index from {at}: {detail}")]
    Fetch {
        /// The URL that failed.
        at: String,
        /// What went wrong.
        detail: String,
    },
    /// The index is not JSON, or not the shape an index has.
    #[error("the index at {at} cannot be read: {detail}")]
    Malformed {
        /// Where the index came from.
        at: String,
        /// What the reader objected to.
        detail: String,
    },
    /// The index announces a format this build does not know.
    #[error(
        "the index at {at} is index-version {theirs}, and this build reads {ours}. \
         Upgrade, or point `registry.url` at an index this version publishes."
    )]
    UnknownVersion {
        /// Where the index came from.
        at: String,
        /// The version it announced.
        theirs: u64,
        /// The version this build reads.
        ours: u64,
    },
    /// An entry is missing a field there is no sensible default for.
    #[error("entry {position} of the index at {at} has no {field}")]
    MissingField {
        /// Where the index came from.
        at: String,
        /// Which entry, counted from 1.
        position: usize,
        /// The field that is absent or the wrong type.
        field: &'static str,
    },
    /// Nothing in the index goes by that name.
    #[error("nothing named {name} in the index at {at} ({count} entries). Try `ext search`.")]
    NotListed {
        /// The name that was asked for.
        name: String,
        /// Where the index came from.
        at: String,
        /// How many entries were there; `0` is the usual cause.
        count: usize,
    },
    /// The caller's digest differs from the index's; refused rather than resolved by precedence.
    #[error(
        "--sha256 {given} disagrees with the index, which lists {listed} for {name}. \
         One of the two is out of date; nothing was fetched."
    )]
    DigestDisagrees {
        /// The component in question.
        name: String,
        /// What the caller passed.
        given: String,
        /// What the index lists.
        listed: String,
    },
    /// The install refused; its refusals already say what to do.
    #[error(transparent)]
    Install(#[from] ExtError),
}

/// Read `registry.url` from the top-level `registry` block; `Ok(None)` when absent.
///
/// # Errors
/// [`IndexError::UrlNotAString`] when `url` is present and not a string.
pub fn url_from_config(registry: Option<&serde_json::Value>) -> Result<Option<String>, IndexError> {
    match registry.and_then(|block| block.get("url")) {
        None => Ok(None),
        Some(serde_json::Value::String(url)) => Ok(Some(url.clone())),
        Some(_) => Err(IndexError::UrlNotAString),
    }
}

/// Read the registry URL straight from a `config.yaml`.
///
/// # Errors
/// [`IndexError::Config`] if unreadable, [`IndexError::NoUrl`] if no registry is named, or [`url_from_config`] refusals.
pub fn url_from_config_path(path: &Path) -> Result<String, IndexError> {
    let registry =
        jk_config::Config::top_level(path, "registry").map_err(|err| IndexError::Config {
            path: path.display().to_string(),
            detail: err.to_string(),
        })?;
    url_from_config(registry.as_ref())?.ok_or_else(|| IndexError::NoUrl {
        path: path.display().to_string(),
    })
}

/// Read the index at a URL or local path; a remote one is egress-checked before any byte moves.
///
/// # Errors
/// [`IndexError::Unreadable`], [`IndexError::Fetch`], [`IndexError::Install`] on a policy refusal, or [`parse`] errors.
pub fn load(source: &str, http: &HttpFn) -> Result<Vec<Entry>, IndexError> {
    let text = if ext::looks_remote(source) {
        ext::check_remote(source)?;
        let response = http("GET", source, &[], None, 0).map_err(|err| IndexError::Fetch {
            at: source.to_owned(),
            detail: format!("{err:?}"),
        })?;
        if response.status != 200 {
            return Err(IndexError::Fetch {
                at: source.to_owned(),
                detail: format!("answered {}", response.status),
            });
        }
        String::from_utf8(response.body).map_err(|err| IndexError::Malformed {
            at: source.to_owned(),
            detail: format!("the response is not UTF-8: {err}"),
        })?
    } else {
        std::fs::read_to_string(source).map_err(|cause| IndexError::Unreadable {
            at: source.to_owned(),
            cause,
        })?
    };
    parse(source, &text)
}

/// Read an index document; `name`, `version`, `api-version`, `kind`, `capabilities`, `url`, `sha256` are required.
///
/// # Errors
/// [`IndexError::Malformed`], [`IndexError::UnknownVersion`], or [`IndexError::MissingField`].
pub fn parse(source: &str, text: &str) -> Result<Vec<Entry>, IndexError> {
    let document: serde_json::Value =
        serde_json::from_str(text).map_err(|err| IndexError::Malformed {
            at: source.to_owned(),
            detail: err.to_string(),
        })?;

    let version = document
        .get("index-version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| IndexError::Malformed {
            at: source.to_owned(),
            detail: "no `index-version` — this does not look like a registry index".to_owned(),
        })?;
    if version != INDEX_VERSION {
        return Err(IndexError::UnknownVersion {
            at: source.to_owned(),
            theirs: version,
            ours: INDEX_VERSION,
        });
    }

    let listed = document
        .get("extensions")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| IndexError::Malformed {
            at: source.to_owned(),
            detail: "no `extensions` array".to_owned(),
        })?;

    let mut entries = Vec::with_capacity(listed.len());
    for (offset, item) in listed.iter().enumerate() {
        let position = offset + 1;
        let required = |field: &'static str| -> Result<String, IndexError> {
            item.get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or(IndexError::MissingField {
                    at: source.to_owned(),
                    position,
                    field,
                })
        };
        let optional = |field: &str| -> String {
            item.get(field)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };

        let capabilities = item
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .ok_or(IndexError::MissingField {
                at: source.to_owned(),
                position,
                field: "capabilities",
            })?
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();

        entries.push(Entry {
            name: required("name")?,
            version: required("version")?,
            api_version: required("api-version")?,
            kind: required("kind")?,
            capabilities,
            description: optional("description"),
            author: optional("author"),
            url: required("url")?,
            sha256: required("sha256")?,
            signature: optional("signature"),
            size: item
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        });
    }
    Ok(entries)
}

/// Entries whose name, kind or description contains `term`, case-insensitively; empty matches all.
#[must_use]
pub fn search<'a>(entries: &'a [Entry], term: &str) -> Vec<&'a Entry> {
    let needle = term.to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            needle.is_empty()
                || entry.name.to_lowercase().contains(&needle)
                || entry.kind.to_lowercase().contains(&needle)
                || entry.description.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The entry named `name`, or a refusal that says how to find one.
///
/// # Errors
/// [`IndexError::NotListed`], with how many entries were searched.
pub fn find<'a>(entries: &'a [Entry], name: &str, source: &str) -> Result<&'a Entry, IndexError> {
    entries
        .iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| IndexError::NotListed {
            name: name.to_owned(),
            at: source.to_owned(),
            count: entries.len(),
        })
}

/// Install the component the index lists under `name`, via [`ext::install_from_url`].
///
/// # Errors
/// [`IndexError::NotListed`], [`IndexError::DigestDisagrees`], or any installer refusal.
pub fn install(
    dir: &Path,
    entries: &[Entry],
    name: &str,
    source: &str,
    checks: &Checks,
    http: &HttpFn,
) -> Result<Installed, IndexError> {
    let entry = find(entries, name, source)?;

    let mut checks = checks.clone();
    match &checks.sha256 {
        Some(given) if !given.eq_ignore_ascii_case(&entry.sha256) => {
            return Err(IndexError::DigestDisagrees {
                name: entry.name.clone(),
                given: given.clone(),
                listed: entry.sha256.clone(),
            })
        }
        Some(_) => {}
        None => checks.sha256 = Some(entry.sha256.clone()),
    }

    Ok(ext::install_from_url(dir, &entry.url, &checks, http)?)
}

#[cfg(test)]
mod tests {
    use super::{find, parse, search, url_from_config, Entry, IndexError, INDEX_VERSION};

    const ONE: &str = r#"{
        "index-version": 1,
        "extensions": [
            {
                "name": "tool-fs", "version": "0.1.0", "api-version": "0.3.0",
                "kind": "tool", "capabilities": ["host-fs", "host-log"],
                "description": "reads and writes inside the workspace",
                "author": "PromptPasture",
                "url": "https://example.com/ext/tool-fs.wasm",
                "sha256": "016481a6eb79d32a82af4a1f7d49af56b11af9431870a857016e4696e62aca26",
                "signature": "", "size": 74057
            }
        ]
    }"#;

    #[test]
    fn an_entry_reads_back_with_what_it_asks_for() {
        let entries = parse("fixture", ONE).expect("parses");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "tool-fs");
        assert_eq!(entries[0].capabilities, ["host-fs", "host-log"]);
        assert_eq!(entries[0].size, 74057);
        assert!(
            entries[0].signature.is_empty(),
            "an empty signature is the honest state until something signs"
        );
    }

    #[test]
    fn the_informational_fields_are_optional() {
        let terse = r#"{"index-version": 1, "extensions": [{
            "name": "tool-x", "version": "0.1.0", "api-version": "0.3.0",
            "kind": "tool", "capabilities": [],
            "url": "https://example.com/tool-x.wasm", "sha256": "abc"
        }]}"#;
        let entries = parse("fixture", terse).expect("parses");
        assert_eq!(entries[0].description, "");
        assert_eq!(entries[0].author, "");
        assert_eq!(entries[0].size, 0);
    }

    #[test]
    fn every_required_field_is_required_and_names_itself() {
        for field in [
            "name",
            "version",
            "api-version",
            "kind",
            "capabilities",
            "url",
            "sha256",
        ] {
            let mut document: serde_json::Value = serde_json::from_str(ONE).expect("parses");
            document["extensions"][0]
                .as_object_mut()
                .expect("an entry is an object")
                .remove(field);
            let text = document.to_string();
            let Err(IndexError::MissingField {
                field: named,
                position,
                ..
            }) = parse("fixture", &text)
            else {
                panic!("an index missing {field} must be refused")
            };
            assert_eq!(named, field, "the refusal names the field that is absent");
            assert_eq!(position, 1, "and which entry it was in");
        }
    }

    #[test]
    fn a_newer_index_format_is_refused_by_name_rather_than_misread() {
        let text = ONE.replace("\"index-version\": 1", "\"index-version\": 2");
        let Err(IndexError::UnknownVersion { theirs, ours, .. }) = parse("fixture", &text) else {
            panic!("an index-version this build does not read must be refused")
        };
        assert_eq!((theirs, ours), (2, INDEX_VERSION));
    }

    #[test]
    fn something_that_is_not_an_index_is_refused_rather_than_read_as_empty() {
        for (text, why) in [
            ("not json at all {{{", "not JSON"),
            (r#"{"extensions": []}"#, "no index-version"),
            (r#"{"index-version": 1}"#, "no extensions array"),
        ] {
            assert!(
                parse("fixture", text).is_err(),
                "{why} must be refused, not read as an empty index"
            );
        }
    }

    fn entry(name: &str, kind: &str, description: &str) -> Entry {
        Entry {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            api_version: "0.3.0".to_owned(),
            kind: kind.to_owned(),
            capabilities: Vec::new(),
            description: description.to_owned(),
            author: String::new(),
            url: String::new(),
            sha256: String::new(),
            signature: String::new(),
            size: 0,
        }
    }

    #[test]
    fn search_matches_name_kind_and_description_case_insensitively() {
        let entries = vec![
            entry("tool-fs", "tool", "reads files"),
            entry("provider-openai", "provider", "OpenAI completions"),
        ];
        let hit = |term: &str| -> Vec<String> {
            search(&entries, term)
                .iter()
                .map(|e| e.name.clone())
                .collect()
        };
        assert_eq!(hit("FS"), ["tool-fs"], "the name, whatever the case");
        assert_eq!(hit("provider"), ["provider-openai"], "the kind");
        assert_eq!(hit("files"), ["tool-fs"], "the description");
        assert_eq!(
            hit("").len(),
            2,
            "an empty term is `list --remote`, not `match nothing`"
        );
        assert_eq!(hit("nothing-like-this"), [] as [std::string::String; 0]);
    }

    #[test]
    fn a_name_that_is_not_listed_says_how_many_were_searched() {
        let entries = vec![entry("tool-fs", "tool", "")];
        let Err(IndexError::NotListed { name, count, .. }) = find(&entries, "tool-sf", "fixture")
        else {
            panic!("a name nothing matches must be refused")
        };
        assert_eq!((name.as_str(), count), ("tool-sf", 1));

        let Err(IndexError::NotListed { count, .. }) = find(&[], "tool-fs", "fixture") else {
            panic!("an empty index cannot satisfy a name either")
        };
        assert_eq!(
            count, 0,
            "0 says the index was empty, not that it was a typo"
        );
    }

    #[test]
    fn a_registry_url_is_read_and_a_non_string_is_refused() {
        let block = serde_json::json!({ "url": "https://example.com/index.json" });
        assert_eq!(
            url_from_config(Some(&block)).expect("reads"),
            Some("https://example.com/index.json".to_owned())
        );
        assert_eq!(
            url_from_config(None).expect("an absent block names no registry"),
            None
        );
        assert_eq!(
            url_from_config(Some(&serde_json::json!({}))).expect("an absent key is not an error"),
            None
        );
        assert!(
            matches!(
                url_from_config(Some(&serde_json::json!({ "url": ["a"] }))),
                Err(IndexError::UrlNotAString)
            ),
            "a malformed value dropped silently would leave an operator believing \
             they had pointed at a registry"
        );
    }
}
