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

use crate::conductor::{ToolInvocation, ToolInvoker};
use crate::ext::{install, Checks};
use crate::intercept::ToolCall;

/// The name the model calls.
const INSTALL: &str = "ext-install";

/// What the model is told this does.
const DESCRIPTION: &str = "Install a WebAssembly extension into the runtime's \
    extension directory. The operator is asked before it happens. An unsigned \
    component requires its sha256; a signed one is verified against the \
    configured trusted keys. The extension is callable after a restart.";

/// The argument schema, matching [`Checks`] rather than inventing a shape.
const SCHEMA: &str = r#"{"type":"object","required":["path"],"properties":{
"path":{"type":"string","description":"the .wasm to install"},
"allow-unsigned":{"type":"boolean","description":"waive the signature; requires sha256"},
"sha256":{"type":"string","description":"expected digest of the component"}}}"#;

/// The host's own tools. Empty unless the operator asked for them.
pub struct NativeTools {
    /// Where components are installed, and `None` when this is off.
    ext_dir: Option<PathBuf>,
    /// Minisign keys from `registry.trusted-keys`. Empty means nothing is
    /// trusted, which is default-deny rather than "skip the check".
    trusted_keys: Vec<String>,
}

impl NativeTools {
    /// Answers nothing, advertises nothing.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            ext_dir: None,
            trusted_keys: Vec::new(),
        }
    }

    /// The install tool, writing into `ext_dir`.
    #[must_use]
    pub const fn installing_into(ext_dir: PathBuf, trusted_keys: Vec<String>) -> Self {
        Self {
            ext_dir: Some(ext_dir),
            trusted_keys,
        }
    }

    /// Metadata for whatever is enabled, in the fleet's advertisement shape.
    #[must_use]
    pub fn metas(&self) -> Vec<serde_json::Value> {
        if self.ext_dir.is_none() {
            return Vec::new();
        }
        vec![serde_json::json!({
            "name": INSTALL,
            "description": DESCRIPTION,
            "parameters-schema": SCHEMA,
        })]
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
        let Some(source) = call.get("path").and_then(serde_json::Value::as_str) else {
            return (
                "refused: `path` is required and must be a string".to_string(),
                true,
            );
        };
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
            Ok(installed) => (
                format!(
                    "installed {} at {}",
                    installed.name,
                    installed.component.display()
                ),
                false,
            ),
            Err(err) => (format!("refused: {err}"), true),
        }
    }
}

impl ToolInvoker for NativeTools {
    fn invoke(&mut self, call: &ToolCall) -> Option<ToolInvocation> {
        // Absent rather than refused when the name is not ours: the fleet
        // chains, and claiming a call we do not serve would stop a guest
        // tool of the same name from ever being reached.
        if call.name != INSTALL {
            return None;
        }
        let (content, failed) = self.run_install(&call.arguments);
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
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![]);
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
        let mut tools = NativeTools::installing_into(dir.clone(), vec![]);
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

    /// Bad arguments are answered, not dropped.
    #[test]
    fn arguments_that_are_not_an_install_say_what_is_wrong() {
        let mut tools = NativeTools::installing_into(std::env::temp_dir(), vec![]);
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
