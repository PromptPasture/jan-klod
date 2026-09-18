//! Tools the host implements itself, rather than loading as components.
//!
//! **There is one, and it exists because the alternatives were worse.**
//! Every other tool in this runtime is a WebAssembly component, and that is
//! a property worth keeping on purpose. Installing an extension cannot be
//! one: a guest that installs needs write access to `ext/`, which is the
//! directory the whole trust boundary is built on, and handing any guest
//! that capability would undo the thing installation is meant to protect
//! (#213).
//!
//! So the host does it, and no guest holds the capability because no guest
//! is involved. The cost is this module — the first tool that is not a
//! component — and it is named in `docs/concepts/architecture.md` rather
//! than left for someone to discover.
//!
//! # Default-deny, like everything else
//!
//! Absent configuration, [`NativeTools::disabled`] answers nothing and
//! advertises nothing: a deployment that did not ask for an install tool
//! does not have one, and the model is not told it exists. `registry:
//! install-tool: true` turns it on.
//!
//! # What still gates it when it is on
//!
//! Two things, both already here rather than invented for this:
//!
//! * **The operator confirms.** `interceptor-permission` confirms any call
//!   not on its read-only allowlist, and this is not on it. The approval an
//!   install needs is the approval path the operator already knows, so this
//!   module adds no second prompt.
//! * **Self-authored is not a trust exemption.** The call goes to
//!   [`crate::ext::install`] with the same [`Checks`] the CLI builds, so an
//!   unsigned artefact still needs its digest and a signed one is still
//!   verified against `registry.trusted-keys`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::conductor::{ToolInvocation, ToolInvoker};
use crate::ext::{install, Checks};
use crate::intercept::ToolCall;

/// The name the model calls to install.
const INSTALL: &str = "ext-install";

/// The name the model calls to see what the registry offers (#218).
const SEARCH: &str = "ext-search";

/// What the model is told `ext-search` does.
const SEARCH_DESCRIPTION: &str = "Search the configured extension registry. \
    Returns each match with the host capabilities it declares, so you can see \
    what a component asks for before proposing it. Read-only: it installs \
    nothing.";

/// `ext-search`'s arguments.
const SEARCH_SCHEMA: &str = r#"{"type":"object","properties":{
"term":{"type":"string","description":"match against name, kind or description; omit for everything"}}}"#;

/// What the model is told this does.
const DESCRIPTION: &str = "Install a WebAssembly extension into the runtime's \
    extension directory, either by `name` from the registry or from a local \
    `path`. The operator is asked before it happens. From the registry the \
    component must be signed, and no waiver is available here. From a local \
    path an unsigned component requires its sha256. The extension is callable \
    from the next turn.";

/// The argument schema, matching [`Checks`] rather than inventing a shape.
const SCHEMA: &str = r#"{"type":"object","properties":{
"name":{"type":"string","description":"a component from the registry, as `ext-search` lists it"},
"path":{"type":"string","description":"a local .wasm; use `name` for anything from the registry"},
"allow-unsigned":{"type":"boolean","description":"local `path` only: waive the signature; requires sha256"},
"sha256":{"type":"string","description":"expected digest, for a local unsigned component"},
"reason":{"type":"string","description":"why you want it, shown to the operator when they are asked; required with `name`"}}}"#;

/// The host's own tools. Empty unless the operator asked for them.
pub struct NativeTools {
    /// Where components are installed, and `None` when this is off.
    ext_dir: Option<PathBuf>,
    /// Minisign keys from `registry.trusted-keys`. Empty means nothing is
    /// trusted, which is default-deny rather than "skip the check".
    trusted_keys: Vec<String>,
    /// How registry fetches are made. `None` uses
    /// [`crate::ext::policy_bound_http`], which is production; a test
    /// injects a canned client so the registry path can be exercised
    /// without opening a socket, the way `ext_registry.rs` already does.
    http: Option<crate::route::HttpFn>,
    /// Where the registry index lives, from `registry.url`. `None` when the
    /// deployment configured none, which makes `ext-search` say so rather
    /// than search nothing and report no matches.
    index_source: Option<String>,
    /// Stems installed during this session, waiting to be adopted (#214).
    ///
    /// Shared rather than returned, because an install happens *inside* a
    /// turn — the tool is dispatched from within the fleet and cannot reach
    /// the `Runtime` that would load it. So it leaves a note, and whoever
    /// drives turns reads it between them.
    installed: Arc<Mutex<Vec<String>>>,
}

impl NativeTools {
    /// Answers nothing, advertises nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            ext_dir: None,
            trusted_keys: Vec::new(),
            http: None,
            index_source: None,
            installed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The install tool, writing into `ext_dir`.
    #[must_use]
    pub fn installing_into(
        ext_dir: PathBuf,
        trusted_keys: Vec<String>,
        index_source: Option<String>,
    ) -> Self {
        Self {
            ext_dir: Some(ext_dir),
            trusted_keys,
            http: None,
            index_source,
            installed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Fetch registry artefacts with `http` instead of the policy-bound
    /// client. Test seam only — production has no caller.
    #[must_use]
    pub fn with_http(mut self, http: crate::route::HttpFn) -> Self {
        self.http = Some(http);
        self
    }

    /// Run `f` with the client registry fetches go through.
    ///
    /// Lent rather than returned: `HttpFn` is a boxed closure and is not
    /// `Clone`, and both `ext_index::load` and `ext::install_from_url` take
    /// it by reference anyway.
    fn with_client<T>(&self, f: impl FnOnce(&crate::route::HttpFn) -> T) -> T {
        match &self.http {
            Some(http) => f(http),
            None => f(&crate::ext::policy_bound_http()),
        }
    }

    /// Take the stems installed since this was last asked.
    ///
    /// Draining rather than reading: adoption happens once per install, and
    /// a list that kept its entries would have the caller adopt the same
    /// component on every turn thereafter — which `adopt_installed` refuses,
    /// loudly, for the rest of the session.
    #[must_use]
    pub fn take_installed(&self) -> Vec<String> {
        self.installed
            .lock()
            .map(|mut queued| std::mem::take(&mut *queued))
            .unwrap_or_default()
    }

    /// Metadata for whatever is enabled, in the fleet's advertisement shape.
    #[must_use]
    pub fn metas(&self) -> Vec<serde_json::Value> {
        if self.ext_dir.is_none() {
            return Vec::new();
        }
        vec![
            serde_json::json!({
                "name": INSTALL,
                "description": DESCRIPTION,
                "parameters-schema": SCHEMA,
            }),
            serde_json::json!({
                "name": SEARCH,
                "description": SEARCH_DESCRIPTION,
                "parameters-schema": SEARCH_SCHEMA,
            }),
        ]
    }

    /// Search the configured registry index.
    ///
    /// # Not on the read-only allowlist, deliberately
    ///
    /// Reading is not an action, so this looks like an obvious candidate for
    /// `interceptor-permission`'s `safe-calls`. It is not one: the index is
    /// usually remote, and that list says of `fetch` that "it's egress, and
    /// a coding agent reaching the network is worth one question". A search
    /// is the same egress wearing a read-only label, so it is confirmed like
    /// any other call. Cheaper to ask once than to find out later that the
    /// allowlist quietly grew a network hole.
    fn run_search(&self, arguments: &str) -> (String, bool) {
        let Some(source) = self.index_source.as_deref() else {
            return (
                "refused: no registry is configured (`registry.url`)".to_string(),
                true,
            );
        };
        let term = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| {
                v.get("term")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let entries = match self.with_client(|http| crate::ext_index::load(source, http)) {
            Ok(entries) => entries,
            Err(err) => return (format!("refused: {err}"), true),
        };
        let hits: Vec<serde_json::Value> = crate::ext_index::search(&entries, &term)
            .into_iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "version": entry.version,
                    "kind": entry.kind,
                    "description": entry.description,
                    // The line the index format exists for: what it asks of
                    // the host, before anybody downloads it.
                    "capabilities": entry.capabilities,
                    // An index entry is *not* necessarily signed — `signature`
                    // is empty when nothing vouches for the bytes. Surfaced
                    // because an unsigned entry cannot be installed through
                    // this tool at all, and a model that proposed one would
                    // be refused for a reason it could have seen here.
                    "signed": !entry.signature.is_empty(),
                })
            })
            .collect();
        (
            serde_json::json!({ "source": source, "matches": hits }).to_string(),
            false,
        )
    }

    /// Install a component the registry lists, by name.
    ///
    /// # No waiver, and what that means
    ///
    /// An index entry is **not** necessarily signed — `Entry::signature` is
    /// empty when nothing vouches for the bytes. This path passes
    /// `allow_unsigned: false` always, so such an entry is refused rather
    /// than installed on the strength of a digest the same index supplied.
    /// A digest from the party serving the artefact answers "did I get what
    /// you sent", not "should I trust the sender".
    ///
    /// So the model has no escape here, by construction: the flag is not
    /// read on this path, and passing it is an error rather than a no-op —
    /// a silently ignored waiver would read, to whoever wrote the call, as
    /// a waiver that worked.
    fn install_from_index(
        &self,
        dir: &Path,
        name: &str,
        call: &serde_json::Value,
    ) -> (String, bool) {
        if call
            .get("allow-unsigned")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return (
                "refused: `allow-unsigned` is for a local `path`; a registry component \
                 is installed on its signature or not at all"
                    .to_string(),
                true,
            );
        }
        // A `reason` is required here and not on the local path, and the
        // asymmetry is deliberate: a path at least names a file the operator
        // can open and read before answering. A registry name is a claim
        // about something remote, and an approval prompt that cannot say
        // *why* is one that gets accepted blindly.
        //
        // It is enforced rather than encouraged, because the prompt is built
        // from the arguments — an absent reason is silently no reason, and
        // the operator would never know one was expected.
        let reason = call
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if reason.is_empty() {
            return (
                "refused: `reason` is required with `name` — the operator is asked to \
                 approve this and needs to know what it is for"
                    .to_string(),
                true,
            );
        }
        let Some(index) = self.index_source.as_deref() else {
            return (
                "refused: no registry is configured (`registry.url`)".to_string(),
                true,
            );
        };
        let entries = match self.with_client(|http| crate::ext_index::load(index, http)) {
            Ok(entries) => entries,
            Err(err) => return (format!("refused: {err}"), true),
        };
        let checks = Checks {
            // No digest of our own: the index carries one and
            // `ext_index::install` refuses a disagreement rather than
            // preferring either. Supplying one here would be inventing a
            // second opinion the model has no way to hold.
            sha256: None,
            trusted_keys: self.trusted_keys.clone(),
            allow_unsigned: false,
        };
        // `ext_index::install`, not a hand-rolled find-and-fetch: it already
        // resolves the name, refuses an unknown one naming the index it
        // searched, and refuses a digest that disagrees with the entry.
        // Reimplementing it here is how those refusals quietly diverge.
        match self.with_client(|http| {
            crate::ext_index::install(dir, &entries, name, index, &checks, http)
        }) {
            Ok(installed) => {
                if let Ok(mut queued) = self.installed.lock() {
                    queued.push(installed.name.clone());
                }
                (
                    format!(
                        "installed {} from the registry; it is callable from the next turn",
                        installed.name
                    ),
                    false,
                )
            }
            Err(err) => (format!("refused: {err}"), true),
        }
    }

    /// Run the install, or say why not.
    ///
    /// Errors are returned as the tool's textual result rather than as a
    /// failure: a model that is told "the digest is required when unsigned"
    /// can supply one, where a model told "tool failed" retries the same
    /// call. `ExtError`'s variants are distinct for exactly this reason.
    fn run_install(&self, arguments: &str) -> (String, bool) {
        let Some(dir) = self.ext_dir.as_deref() else {
            return ("refused: the install tool is not enabled".to_string(), true);
        };
        let call: serde_json::Value = match serde_json::from_str(arguments) {
            Ok(value) => value,
            Err(err) => return (format!("refused: arguments are not JSON ({err})"), true),
        };
        let named = call.get("name").and_then(serde_json::Value::as_str);
        let path = call.get("path").and_then(serde_json::Value::as_str);
        // Exactly one. A call carrying both is two different requests, and
        // picking one silently is how a typo in `name` becomes an arbitrary
        // fetch from whatever `path` happened to say.
        let Some(source) = (match (named, path) {
            (Some(_), Some(_)) => {
                return (
                    "refused: give `name` (from the registry) or `path` (local), not both"
                        .to_string(),
                    true,
                )
            }
            (None, None) => {
                return (
                    "refused: `name` (from the registry) or `path` (local) is required".to_string(),
                    true,
                )
            }
            (named, path) => named.or(path),
        }) else {
            unreachable!("the match above returns in both empty cases")
        };
        if let Some(name) = named {
            return self.install_from_index(dir, name, &call);
        }
        let checks = Checks {
            sha256: call
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            trusted_keys: self.trusted_keys.clone(),
            allow_unsigned: call
                .get("allow-unsigned")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        };
        match install(dir, Path::new(source), &checks) {
            Ok(installed) => {
                if let Ok(mut queued) = self.installed.lock() {
                    queued.push(installed.name.clone());
                }
                (
                    format!(
                        "installed {} at {}; it is callable from the next turn",
                        installed.name,
                        installed.component.display()
                    ),
                    false,
                )
            }
            Err(err) => (format!("refused: {err}"), true),
        }
    }
}

impl ToolInvoker for NativeTools {
    fn invoke(&mut self, call: &ToolCall) -> Option<ToolInvocation> {
        // Absent rather than refused when the name is not ours: the fleet
        // chains, and claiming a call we do not serve would stop a guest
        // tool of the same name from ever being reached.
        let (content, failed) = match call.name.as_str() {
            INSTALL => self.run_install(&call.arguments),
            SEARCH => self.run_search(&call.arguments),
            _ => return None,
        };
        Some(ToolInvocation { content, failed })
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeTools, INSTALL};
    use crate::conductor::ToolInvoker;
    use crate::intercept::ToolCall;

    fn call(arguments: &str) -> ToolCall {
        ToolCall {
            id: "c1".to_string(),
            name: INSTALL.to_string(),
            arguments: arguments.to_string(),
        }
    }

    /// Off by default, and *absent* rather than refusing — a deployment that
    /// did not ask for an install tool does not have one.
    #[test]
    fn disabled_advertises_nothing() {
        assert!(NativeTools::disabled().metas().is_empty());
    }

    /// A call that is not ours is `None`, not a refusal. Claiming it would
    /// stop the fleet's next link from ever seeing it.
    #[test]
    fn another_tools_call_is_passed_on_rather_than_claimed() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let mut other = call("{}");
        other.name = "fs".to_string();
        assert!(tools.invoke(&other).is_none());
    }

    /// The one refusal this module owns rather than delegates: the tool is
    /// wired but off, so a call must say so rather than install anything.
    #[test]
    fn a_call_while_disabled_installs_nothing_and_says_so() {
        let mut tools = NativeTools::disabled();
        let out = tools
            .invoke(&call(r#"{"path":"/nope.wasm"}"#))
            .expect("ours");
        assert!(out.content.starts_with("refused"), "{}", out.content);
    }

    /// Self-authored is not a trust exemption: the refusal comes from
    /// `ext::install`, unchanged, and reaches the model as text it can act
    /// on rather than an opaque failure.
    #[test]
    fn unsigned_without_a_digest_is_refused_through_the_tool() {
        let dir = std::env::temp_dir().join(format!("jk-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates the dir");
        let mut tools = NativeTools::installing_into(dir.clone(), vec![], None);
        let out = tools
            .invoke(&call(
                r#"{"path":"/nonexistent.wasm","allow-unsigned":true}"#,
            ))
            .expect("ours");
        assert!(
            out.content.contains("digest"),
            "the refusal should name the missing digest, not the missing file: {}",
            out.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Both tools are offered when enabled, and neither when not.
    #[test]
    fn enabling_offers_search_as_well_as_install() {
        let names: Vec<String> = NativeTools::installing_into(std::env::temp_dir(), vec![], None)
            .metas()
            .iter()
            .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(str::to_owned))
            .collect();
        assert_eq!(names, vec!["ext-install", "ext-search"]);
    }

    /// Searching with no registry configured says so, rather than reporting
    /// no matches — "nothing found" and "nowhere to look" are different
    /// facts and a model cannot act on the wrong one.
    #[test]
    fn searching_with_no_registry_configured_says_so() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let mut search = call("{}");
        search.name = "ext-search".to_string();
        let out = tools.invoke(&search).expect("ours");
        assert!(out.failed);
        assert!(
            out.content.contains("no registry is configured"),
            "{}",
            out.content
        );
    }

    /// The waiver is refused on the registry path, not ignored.
    ///
    /// Silently ignoring it would read, to whoever wrote the call, as a
    /// waiver that worked — and the next thing they write will rely on it.
    #[test]
    fn allow_unsigned_is_refused_for_a_registry_install() {
        let mut tools = NativeTools::installing_into(
            std::env::temp_dir(),
            vec![],
            Some("/nonexistent/index.json".to_string()),
        );
        let out = tools
            .invoke(&call(r#"{"name":"tool-x","allow-unsigned":true}"#))
            .expect("ours");
        assert!(out.failed);
        assert!(
            out.content.contains("on its signature or not at all"),
            "{}",
            out.content
        );
    }

    /// `name` and `path` are two different requests, so both together is an
    /// error rather than a preference.
    #[test]
    fn naming_both_a_registry_component_and_a_path_is_refused() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let out = tools
            .invoke(&call(r#"{"name":"tool-x","path":"/tmp/x.wasm"}"#))
            .expect("ours");
        assert!(out.failed);
        assert!(out.content.contains("not both"), "{}", out.content);
    }

    /// Neither is also an error, and it names both ways in rather than
    /// only the one it happened to check first.
    #[test]
    fn naming_neither_says_what_is_missing() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let out = tools.invoke(&call("{}")).expect("ours");
        assert!(out.failed);
        assert!(out.content.contains("`name`"), "{}", out.content);
        assert!(out.content.contains("`path`"), "{}", out.content);
    }

    /// Bad arguments are answered, not dropped.
    #[test]
    fn arguments_that_are_not_an_install_say_what_is_wrong() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        assert!(tools
            .invoke(&call("not json"))
            .expect("ours")
            .content
            .contains("not JSON"));
        assert!(tools
            .invoke(&call("{}"))
            .expect("ours")
            .content
            .contains("`path`"));
    }
}
