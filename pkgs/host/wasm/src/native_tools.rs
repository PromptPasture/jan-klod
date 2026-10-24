//! Tools the host implements itself rather than as components: install (#213) and `ext-search`.
//!
//! Installing needs write access to `ext/`, the trust boundary, so no guest may hold it. Default-deny:
//! [`NativeTools::disabled`] advertises nothing; `registry: install-tool: true` enables it. Still gated
//! by `interceptor-permission` confirmation and by [`crate::ext::install`] with the CLI's [`Checks`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::ext::{install, Checks};
use jk_protocol::intercept::{ToolInvocation, ToolInvoker};
use jk_protocol::turn::ToolCall;

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
    /// Minisign keys from `registry.trusted-keys`; empty trusts nothing.
    trusted_keys: Vec<String>,
    /// Registry fetch client; `None` is the policy-bound production client, tests inject a canned one.
    http: Option<crate::route::HttpFn>,
    /// Registry index location (`registry.url`); `None` makes `ext-search` say so, not report no matches.
    index_source: Option<String>,
    /// Stems installed this session awaiting adoption (#214); shared because the tool runs inside a
    /// turn and cannot reach the `Runtime` that loads it.
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

    /// Fetch registry artefacts with `http` instead of the policy-bound client. Test seam only.
    #[must_use]
    pub fn with_http(mut self, http: crate::route::HttpFn) -> Self {
        self.http = Some(http);
        self
    }

    /// Run `f` with the client registry fetches go through (lent: `HttpFn` is not `Clone`).
    fn with_client<T>(&self, f: impl FnOnce(&crate::route::HttpFn) -> T) -> T {
        match &self.http {
            Some(http) => f(http),
            None => f(&crate::ext::policy_bound_http()),
        }
    }

    /// Take the stems installed since last asked; draining so the same component is not re-adopted every turn.
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
    /// Not on `interceptor-permission`'s `safe-calls`: the index is usually remote, so this is egress
    /// and is confirmed like any other call.
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
                    // What the entry asks of the host, shown before download.
                    "capabilities": entry.capabilities,
                    // `signature` may be empty: an unsigned entry cannot be installed here, so surface that up front.
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
    /// Always `allow_unsigned: false`: a digest from the party serving the artefact does not establish
    /// trust in it. Passing the flag is an error, not a no-op, so a waiver never silently appears to work.
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
        // `reason` is required here (not on the local path): a remote name is a claim the operator
        // cannot inspect, and the prompt is built from arguments so an absent reason would go unnoticed.
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
            // No digest of our own: `ext_index::install` refuses disagreement with the index's.
            sha256: None,
            trusted_keys: self.trusted_keys.clone(),
            allow_unsigned: false,
        };
        // Reuse `ext_index::install` (name resolution, unknown-name and digest-mismatch refusals) rather than reimplementing.
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
    /// Errors are returned as the tool's text so the model can act on them (e.g. supply a digest) instead of retrying.
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
        // Exactly one of name/path: picking silently turns a typo into an arbitrary fetch.
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
        // Absent, not refused, for names not ours: the fleet chains and claiming would shadow a guest tool.
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
    use jk_protocol::intercept::ToolInvoker;
    use jk_protocol::turn::ToolCall;

    fn call(arguments: &str) -> ToolCall {
        ToolCall {
            id: "c1".to_string(),
            name: INSTALL.to_string(),
            arguments: arguments.to_string(),
        }
    }

    /// Off by default and absent rather than refusing.
    #[test]
    fn disabled_advertises_nothing() {
        assert_eq!(
            NativeTools::disabled().metas(),
            [] as [serde_json::Value; 0]
        );
    }

    /// A call that is not ours is `None`, so the fleet's next link sees it.
    #[test]
    fn another_tools_call_is_passed_on_rather_than_claimed() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let mut other = call("{}");
        other.name = "fs".to_string();
        assert!(tools.invoke(&other).is_none());
    }

    /// The one refusal this module owns: the tool is wired but off.
    #[test]
    fn a_call_while_disabled_installs_nothing_and_says_so() {
        let mut tools = NativeTools::disabled();
        let out = tools
            .invoke(&call(r#"{"path":"/nope.wasm"}"#))
            .expect("ours");
        assert!(out.content.starts_with("refused"), "{}", out.content);
    }

    /// A bad name is refused with text listing what names may be (#222); the component is empty
    /// because the stem is read before its bytes.
    #[test]
    fn a_stem_naming_no_category_is_refused_through_the_tool() {
        let dir = std::env::temp_dir().join(format!("jk-native-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("creates the dir");
        let source = dir.join("self-built.wasm");
        std::fs::write(&source, b"").expect("writes the offer");
        let mut tools = NativeTools::installing_into(dir.clone(), vec![], None);
        let out = tools
            .invoke(&call(&format!(
                r#"{{"path":"{}","allow-unsigned":true,"sha256":"00"}}"#,
                source.display()
            )))
            .expect("ours");
        assert!(out.failed, "{}", out.content);
        for category in crate::CATEGORIES {
            assert!(
                out.content.contains(category),
                "the model is told `{category}` is available: {}",
                out.content
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Self-authored is not a trust exemption: `ext::install`'s refusal reaches the model as text.
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

    /// Searching with no registry configured says so rather than reporting no matches.
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

    /// `name` and `path` together is an error, not a preference.
    #[test]
    fn naming_both_a_registry_component_and_a_path_is_refused() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![], None);
        let out = tools
            .invoke(&call(r#"{"name":"tool-x","path":"/tmp/x.wasm"}"#))
            .expect("ours");
        assert!(out.failed);
        assert!(out.content.contains("not both"), "{}", out.content);
    }

    /// Neither is also an error, naming both ways in.
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
