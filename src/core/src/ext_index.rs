//! The registry index: what `ext search` reads, and what `ext install <name>`
//! resolves a name through.
//!
//! # Why this module is not called `registry`
//!
//! That word is taken twice already, and both are something else.
//! [`crate::registry_host`] is the host side of the `extensions.registry`
//! category (the `skills` and `mcp` catalogues), and the top-level `registry`
//! block in `config.yaml` holds the `trusted-keys` grant. This is the third
//! thing wearing the name — a published list of components — so it is named for
//! what it is instead: the index `ext` commands read.
//!
//! # What an index is for
//!
//! A manifest states what a component asks the host for. An index carries that
//! statement **away from the bytes**, so an operator can read it before
//! downloading anything. That is the whole point: `ext search` prints the
//! capability line, and until an interactive Configurator exists it *is* the
//! capability view.
//!
//! # What it is not trusted for
//!
//! Nothing here decides whether a component may be installed. The entry's
//! `sha256` becomes the digest the existing install flow checks, and the
//! signature is still verified against `registry.trusted-keys` over the fetched
//! bytes — so an index that lies about a component produces a refusal at
//! install time rather than a component nobody vouched for. An index is a
//! directory, not an authority.

use std::path::Path;

use crate::ext::{self, Checks, ExtError, Installed};
use crate::route::HttpFn;

/// The index format this build understands.
///
/// Refused rather than tolerated when it differs: a reader that guessed at a
/// shape it does not know would report missing fields instead of "this index is
/// newer than me", which is the fix the operator needs.
pub const INDEX_VERSION: u64 = 1;

/// One published component, as the index describes it.
///
/// The same shape `src/core/examples/registry_index.rs` writes — one
/// format, written in one place and read in one place.
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
    /// The `host-*` interfaces it declares — the line this whole format exists
    /// to put in front of an operator before a download.
    pub capabilities: Vec<String>,
    /// What it is for, in prose. Empty when the index says nothing.
    pub description: String,
    /// Who published it. Empty when the index says nothing.
    pub author: String,
    /// Where the `.wasm` is served from. The manifest and signature are
    /// siblings of it, which is the convention [`ext::install_from_url`]
    /// already derives.
    pub url: String,
    /// Expected SHA-256 of the component, as hex.
    pub sha256: String,
    /// The detached minisign signature over the component, when the index
    /// carries one. Empty means **nothing vouches for these bytes**, which
    /// `ext search` says out loud rather than leaving blank.
    pub signature: String,
    /// The component's size in bytes, or `0` when the index does not say.
    pub size: u64,
}

/// Why an index could not be used.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// `registry.url` is absent, so there is no index to read.
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
        /// Which entry, counted from 1 — an index may well have no names left
        /// to identify it by.
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
        /// How many entries were there — `0` is the usual cause and says so.
        count: usize,
    },
    /// The caller passed a digest, and the index states a different one.
    ///
    /// Refused rather than resolved by precedence. Two claims about the same
    /// bytes disagreeing is a fact worth stopping on: silently preferring
    /// either one would check bytes against a digest the operator did not think
    /// they had asked for.
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
    /// The install itself refused. Passed through rather than reworded — the
    /// installer's refusals already name what was wrong and what to do.
    #[error(transparent)]
    Install(#[from] ExtError),
}

/// Read `registry.url` from the top-level `registry` block.
///
/// `Ok(None)` means the key is absent, which is a legitimate state — an
/// operator who has named no registry has one fewer way to install things, not
/// a broken config. The caller decides whether its command needs one.
///
/// The **top-level** `registry`, not the `extensions.registry` category that
/// holds `skills` and `mcp`, the same distinction [`Checks::from_config`]
/// draws.
///
/// # Errors
/// [`IndexError::UrlNotAString`] when `url` is present and is not a string.
/// Strict for the reason the trusted-keys reader is: a malformed value silently
/// dropped would leave an operator believing they had pointed at a registry.
pub fn url_from_config(registry: Option<&serde_json::Value>) -> Result<Option<String>, IndexError> {
    match registry.and_then(|block| block.get("url")) {
        None => Ok(None),
        Some(serde_json::Value::String(url)) => Ok(Some(url.clone())),
        Some(_) => Err(IndexError::UrlNotAString),
    }
}

/// Read the registry URL straight from a `config.yaml`.
///
/// Here rather than in the gateway binary for the reason
/// [`Checks::from_config_path`] is: this crate already depends on the config
/// crate and the binary does not, so a CLI that parsed config would be a second
/// reader of it.
///
/// # Errors
/// [`IndexError::Config`] if the file cannot be read or parsed,
/// [`IndexError::NoUrl`] when it names no registry, and whatever
/// [`url_from_config`] refuses.
pub fn url_from_config_path(path: &Path) -> Result<String, IndexError> {
    // `top_level` rather than `from_path`, for the same reason `ext install`
    // uses it: the latter expands `${VAR}` in every enabled instance, so
    // searching a registry would demand a provider's API key.
    let registry =
        jan_klod_config::Config::top_level(path, "registry").map_err(|err| IndexError::Config {
            path: path.display().to_string(),
            detail: err.to_string(),
        })?;
    url_from_config(registry.as_ref())?.ok_or_else(|| IndexError::NoUrl {
        path: path.display().to_string(),
    })
}

/// Read the index named by `source` — a URL, or a path to a local file.
///
/// The same rule [`ext::looks_remote`] applies to an install source, and for
/// the same reason: `http`/`https` fetches, everything else is a path. That is
/// what lets an operator point `registry.url` at a file and work entirely
/// offline, and what lets a test do the same without a socket.
///
/// A remote index is checked against the egress policy **before a byte moves**,
/// exactly as a remote component is. An index is a list of places to download
/// executable code from; it is not a lesser destination than the code itself.
///
/// # Errors
/// [`IndexError::Unreadable`] for a local file, [`IndexError::Fetch`] for a
/// request that fails or answers non-200, [`IndexError::Install`] carrying
/// [`ExtError::RefusedByPolicy`] for a destination the policy denies, and
/// whatever [`parse`] refuses about the content.
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

/// Read an index document.
///
/// Fields are read off a `serde_json::Value` rather than deserialized into a
/// struct, matching how [`crate::manifest`] reads a manifest and how the rest
/// of the core reads opaque config. It also buys the error messages: a
/// derive would report "missing field `sha256`" for a document it could not
/// name the position in.
///
/// **Required: `name`, `version`, `api-version`, `kind`, `capabilities`,
/// `url`, `sha256`.** Those are what it takes to decide about a component and
/// to verify it. `description`, `author` and `signature` default to empty and
/// `size` to `0` — they inform, and an index that omits one is terse rather
/// than broken.
///
/// # Errors
/// [`IndexError::Malformed`] for anything that is not a JSON document with an
/// `extensions` array, [`IndexError::UnknownVersion`] for a format this build
/// does not read, and [`IndexError::MissingField`] naming the entry and the
/// field.
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

/// Entries whose name, kind or description contains `term`, case-insensitively.
///
/// An empty term matches everything, which is what makes `ext list --remote`
/// this function with nothing to filter by rather than a second traversal.
///
/// Order is the index's own, which the generator sorts by name.
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
/// [`IndexError::NotListed`], naming how many entries were searched — `0`
/// means the index was empty, which is a different problem from a typo.
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

/// Install the component the index lists under `name`.
///
/// The index contributes **where the bytes are and what their digest should
/// be**, and nothing else: the fetch, the signature check against
/// `registry.trusted-keys`, the manifest cross-check and the staged landing are
/// all [`ext::install_from_url`], unchanged. There is no second, weaker install
/// path to keep in step with the first — the same reason the URL install was
/// built as a fetch in front of [`ext::install`] rather than beside it.
///
/// # Errors
/// [`IndexError::NotListed`] when no entry has that name,
/// [`IndexError::DigestDisagrees`] when the caller passed a digest the index
/// contradicts, and [`IndexError::Install`] for every refusal the installer can
/// produce.
pub fn install(
    dir: &Path,
    entries: &[Entry],
    name: &str,
    source: &str,
    checks: &Checks,
    http: &HttpFn,
) -> Result<Installed, IndexError> {
    let entry = find(entries, name, source)?;

    // A digest that reached the operator by another route is the strongest
    // evidence here, so a caller's `--sha256` is not overridden — but it is not
    // silently preferred either. Disagreement is refused above.
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

    /// The terse case: an index that states only what it takes to decide and
    /// verify is a legitimate index, not a broken one.
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

    /// Each required field, dropped one at a time. A reader that filled in a
    /// default for any of these would install something nobody described.
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

    /// Three documents that are JSON and are not an index. Each has to be a
    /// refusal an operator can act on rather than an empty result, which would
    /// read as "the registry has nothing".
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
        assert!(hit("nothing-like-this").is_empty());
    }

    /// An empty index and a typo are different problems, and the count is what
    /// tells them apart.
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
