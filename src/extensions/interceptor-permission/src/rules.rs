//! Permission rules — pure Rust, unit-tested natively.
//!
//! Three independent checks gate a tool call, all driven by a [`Policy`] that the
//! component builds from its `config.yaml` section (falling back to the built-in
//! defaults when a key is absent):
//!
//! 1. **Name check** ([`Policy::is_dangerous`]): the tool name contains a
//!    high-risk verb (e.g. `shell`, `exec`, `write`). Name-based, no argument
//!    parsing.
//!
//! 2. **Op check** ([`Policy::args_are_dangerous`]): a benign-named multi-op tool
//!    (e.g. the unified `fs` tool) selects a mutating operation via
//!    `{"op":"write"}`.
//!
//! 3. **Scope check** ([`Policy::args_escape_scope`]): any string argument
//!    contains a path that would leave the workspace — either an absolute path
//!    (`/…`) or a component that traverses upward (`..`). This catches a
//!    non-dangerous tool name (e.g. `fs-read`) being called with `../../etc/passwd`.
//!
//! Each list-valued policy field, when present in config, **replaces** the
//! built-in default; a missing key keeps the default. The two scope checks are
//! toggled independently.

/// Built-in high-risk name substrings: a tool whose name contains any of these
/// prompts a confirmation. Kept lowercase; matching lower-cases the tool name
/// first. Used when config omits `dangerous-names`.
const DEFAULT_DANGEROUS_NAMES: &[&str] = &[
    "bash", "shell", "exec", "eval", "rm", "delete", "remove", "write", "kill", "sudo",
];

/// Built-in high-risk `op` values: a multi-op tool (e.g. `fs`) whose `op`
/// argument is one of these is treated as dangerous even though its name is
/// benign. Kept lowercase. Used when config omits `dangerous-ops`.
const DEFAULT_DANGEROUS_OPS: &[&str] = &["write", "delete", "remove", "exec", "run"];

/// A resolved permission policy: the three checks read from these fields rather
/// than module constants, so the same rules serve both the built-in defaults and
/// a config-driven override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Substrings that make a tool name high-risk (lowercase).
    dangerous_names: Vec<String>,
    /// `op` argument values that make a multi-op call high-risk (lowercase).
    dangerous_ops: Vec<String>,
    /// When true, absolute-path arguments do not trip the scope check.
    allow_absolute_paths: bool,
    /// When true, `..` traversal in path arguments does not trip the scope check.
    allow_parent_traversal: bool,
}

impl Default for Policy {
    /// The built-in policy: the historical hardcoded lists, both scope checks on.
    fn default() -> Self {
        Self {
            dangerous_names: DEFAULT_DANGEROUS_NAMES.iter().map(|s| (*s).to_owned()).collect(),
            dangerous_ops: DEFAULT_DANGEROUS_OPS.iter().map(|s| (*s).to_owned()).collect(),
            allow_absolute_paths: false,
            allow_parent_traversal: false,
        }
    }
}

impl Policy {
    /// Build a policy from an extension config section (the JSON object served by
    /// `host-config::all`). Recognised keys, each optional:
    ///
    /// - `dangerous-names`: array of strings — **replaces** the default name list.
    /// - `dangerous-ops`: array of strings — **replaces** the default op list.
    /// - `allow-absolute-paths`: bool — default `false`.
    /// - `allow-parent-traversal`: bool — default `false`.
    ///
    /// A missing or wrong-typed key falls back to the default. String lists are
    /// lower-cased so matching stays case-insensitive.
    #[must_use]
    pub fn from_config(section: &serde_json::Value) -> Self {
        let default = Self::default();
        Self {
            dangerous_names: string_list(section, "dangerous-names")
                .unwrap_or(default.dangerous_names),
            dangerous_ops: string_list(section, "dangerous-ops").unwrap_or(default.dangerous_ops),
            allow_absolute_paths: section
                .get("allow-absolute-paths")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default.allow_absolute_paths),
            allow_parent_traversal: section
                .get("allow-parent-traversal")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default.allow_parent_traversal),
        }
    }

    /// Whether a tool call to `name` should be confirmed before running.
    #[must_use]
    pub fn is_dangerous(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.dangerous_names.iter().any(|verb| name.contains(verb.as_str()))
    }

    /// Whether the JSON `arguments` select a high-risk operation via an `op` field
    /// (e.g. the unified `fs` tool called with `{"op":"write",…}`). This gates a
    /// benign-named multi-op tool whose mutating mode would otherwise slip past the
    /// name check.
    ///
    /// Non-JSON or non-object arguments (and a missing/benign `op`) are not flagged.
    #[must_use]
    pub fn args_are_dangerous(&self, arguments: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
            return false;
        };
        value
            .get("op")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|op| self.dangerous_ops.contains(&op.to_lowercase()))
    }

    /// Whether any string value in the JSON `arguments` object contains a path that
    /// escapes the workspace: an absolute path (starts with `/`) or a component that
    /// traverses upward (`..`). Either check can be disabled via policy toggles.
    ///
    /// Non-JSON or non-object arguments are treated as safe (the name check or the
    /// tool itself will reject them).
    #[must_use]
    pub fn args_escape_scope(&self, arguments: &str) -> bool {
        // Both toggles on -> nothing to check.
        if self.allow_absolute_paths && self.allow_parent_traversal {
            return false;
        }
        let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(arguments)
        else {
            return false;
        };
        string_values_recursive(&serde_json::Value::Object(map))
            .iter()
            .any(|s| self.path_escapes(s))
    }

    /// Whether a path string escapes the workspace root, honouring the toggles.
    fn path_escapes(&self, s: &str) -> bool {
        if !self.allow_absolute_paths && s.starts_with('/') {
            return true;
        }
        if !self.allow_parent_traversal && s.split('/').any(|component| component == "..") {
            return true;
        }
        false
    }
}

/// Read `key` as an array of strings, lower-cased. Returns `None` when the key is
/// absent or not an array (so callers fall back to a default).
fn string_list(section: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let arr = section.get(key)?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_lowercase))
            .collect(),
    )
}

/// Recursively collect all string leaf values from a JSON value.
fn string_values_recursive(value: &serde_json::Value) -> Vec<&str> {
    match value {
        serde_json::Value::String(s) => vec![s.as_str()],
        serde_json::Value::Array(arr) => arr.iter().flat_map(string_values_recursive).collect(),
        serde_json::Value::Object(map) => {
            map.values().flat_map(string_values_recursive).collect()
        }
        _ => vec![],
    }
}

/// Whether the driver's answer approves the call. Anything else denies it — the
/// safe default for a permission gate.
#[must_use]
pub fn is_affirmative(answer: &str) -> bool {
    matches!(
        answer.trim().to_lowercase().as_str(),
        "y" | "yes" | "allow" | "approve" | "ok"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dangerous_verbs_are_flagged() {
        let policy = Policy::default();
        for name in ["bash", "run_shell", "fs.delete", "exec_code", "rm-rf", "sudo_apt"] {
            assert!(policy.is_dangerous(name), "{name:?} should require confirmation");
        }
    }

    #[test]
    fn ordinary_tools_are_not_flagged() {
        let policy = Policy::default();
        for name in ["web_search", "read_file", "fetch", "list_dir", "calculator"] {
            assert!(!policy.is_dangerous(name), "{name:?} should not require confirmation");
        }
    }

    #[test]
    fn only_explicit_yes_approves() {
        for yes in ["y", "Yes", " ALLOW ", "approve", "ok"] {
            assert!(is_affirmative(yes), "{yes:?} should approve");
        }
        for no in ["", "n", "no", "nope", "cancel", "later", "maybe"] {
            assert!(!is_affirmative(no), "{no:?} should deny");
        }
    }

    #[test]
    fn dangerous_op_arg_is_flagged() {
        let policy = Policy::default();
        for args in [
            r#"{"op": "write", "path": "out.txt", "contents": "x"}"#,
            r#"{"op": "delete", "path": "out.txt"}"#,
            r#"{"op": "EXEC", "path": "x"}"#,
        ] {
            assert!(policy.args_are_dangerous(args), "{args:?} should be flagged as a high-risk op");
        }
    }

    #[test]
    fn benign_op_arg_is_safe() {
        let policy = Policy::default();
        for args in [
            r#"{"op": "read", "path": "src/main.rs"}"#,
            r#"{"op": "grep", "path": "src/main.rs", "pattern": "fn"}"#,
            r#"{"path": "src/main.rs"}"#,
            "not json",
        ] {
            assert!(!policy.args_are_dangerous(args), "{args:?} should not be flagged");
        }
    }

    #[test]
    fn traversal_in_path_arg_is_flagged() {
        let policy = Policy::default();
        for args in [
            r#"{"path": "../../etc/passwd"}"#,
            r#"{"path": "../sibling"}"#,
            r#"{"path": "a/b/../../secret"}"#,
            r#"{"file": "foo/../../../root"}"#,
        ] {
            assert!(policy.args_escape_scope(args), "{args:?} should be flagged as scope escape");
        }
    }

    #[test]
    fn absolute_path_arg_is_flagged() {
        let policy = Policy::default();
        for args in [
            r#"{"path": "/etc/passwd"}"#,
            r#"{"path": "/home/user/.ssh/id_rsa"}"#,
            r#"{"args": ["/bin/sh", "-c", "whoami"]}"#,
        ] {
            assert!(policy.args_escape_scope(args), "{args:?} should be flagged as absolute path");
        }
    }

    #[test]
    fn workspace_relative_paths_are_safe() {
        let policy = Policy::default();
        for args in [
            r#"{"path": "src/main.rs"}"#,
            r#"{"path": "subdir/file.txt"}"#,
            r#"{"path": "..hidden_file"}"#,
            r#"{"command": "echo", "args": ["hello"]}"#,
        ] {
            assert!(!policy.args_escape_scope(args), "{args:?} should not be flagged");
        }
    }

    #[test]
    fn non_json_and_non_object_args_are_safe() {
        let policy = Policy::default();
        for args in ["", "not json", r#""a string""#, "42", "null"] {
            assert!(!policy.args_escape_scope(args), "{args:?} should not be flagged");
        }
    }

    #[test]
    fn empty_config_yields_defaults() {
        assert_eq!(Policy::from_config(&json!({})), Policy::default());
    }

    #[test]
    fn config_replaces_name_list() {
        let policy = Policy::from_config(&json!({ "dangerous-names": ["danger", "NUKE"] }));
        assert!(policy.is_dangerous("nuke_everything"));
        assert!(policy.is_dangerous("some_danger_zone"));
        // Old defaults no longer apply — the list is replaced, not extended.
        assert!(!policy.is_dangerous("bash"));
        assert!(!policy.is_dangerous("rm-rf"));
    }

    #[test]
    fn config_replaces_op_list() {
        let policy = Policy::from_config(&json!({ "dangerous-ops": ["purge"] }));
        assert!(policy.args_are_dangerous(r#"{"op": "purge"}"#));
        // Default ops are gone.
        assert!(!policy.args_are_dangerous(r#"{"op": "write"}"#));
    }

    #[test]
    fn empty_list_disables_a_check() {
        let policy = Policy::from_config(&json!({ "dangerous-names": [] }));
        assert!(!policy.is_dangerous("bash"));
        assert!(!policy.is_dangerous("rm"));
    }

    #[test]
    fn scope_toggles_relax_checks() {
        let abs_ok = Policy::from_config(&json!({ "allow-absolute-paths": true }));
        assert!(!abs_ok.args_escape_scope(r#"{"path": "/etc/passwd"}"#));
        // Traversal still gated.
        assert!(abs_ok.args_escape_scope(r#"{"path": "../secret"}"#));

        let trav_ok = Policy::from_config(&json!({ "allow-parent-traversal": true }));
        assert!(!trav_ok.args_escape_scope(r#"{"path": "../secret"}"#));
        // Absolute still gated.
        assert!(trav_ok.args_escape_scope(r#"{"path": "/etc/passwd"}"#));

        let both = Policy::from_config(
            &json!({ "allow-absolute-paths": true, "allow-parent-traversal": true }),
        );
        assert!(!both.args_escape_scope(r#"{"path": "/etc/passwd"}"#));
        assert!(!both.args_escape_scope(r#"{"path": "../secret"}"#));
    }

    #[test]
    fn wrong_typed_keys_fall_back_to_defaults() {
        let policy = Policy::from_config(
            &json!({ "dangerous-names": "not-an-array", "allow-absolute-paths": "yes" }),
        );
        assert_eq!(policy, Policy::default());
    }
}
