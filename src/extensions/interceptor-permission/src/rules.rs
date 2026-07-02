//! Permission rules — pure Rust, unit-tested natively.
//!
//! Three independent checks gate a tool call:
//!
//! 1. **Name check** ([`is_dangerous`]): the tool name contains a high-risk verb
//!    (e.g. `shell`, `exec`, `write`). Name-based, no argument parsing.
//!
//! 2. **Op check** ([`args_are_dangerous`]): a benign-named multi-op tool (e.g.
//!    the unified `fs` tool) selects a mutating operation via `{"op":"write"}`.
//!
//! 3. **Scope check** ([`args_escape_scope`]): any string argument contains a
//!    path that would leave the workspace — either an absolute path (`/…`) or a
//!    component that traverses upward (`..`). This catches a non-dangerous tool
//!    name (e.g. `fs-read`) being called with `../../etc/passwd`.
//!
//! A config-driven policy engine is a later refinement; these two rules cover the
//! critical failure modes for a single-workspace agent.

/// High-risk substrings: a tool whose name contains any of these prompts a
/// confirmation. Kept lowercase; matching lower-cases the tool name first.
const DANGEROUS: &[&str] = &[
    "bash", "shell", "exec", "eval", "rm", "delete", "remove", "write", "kill", "sudo",
];

/// High-risk `op` values: a multi-op tool (e.g. `fs`) whose `op` argument is one of
/// these is treated as dangerous even though its name is benign. Kept lowercase.
const DANGEROUS_OPS: &[&str] = &["write", "delete", "remove", "exec", "run"];

/// Whether a tool call to `name` should be confirmed before running.
#[must_use]
pub fn is_dangerous(name: &str) -> bool {
    let name = name.to_lowercase();
    DANGEROUS.iter().any(|verb| name.contains(verb))
}

/// Whether the JSON `arguments` select a high-risk operation via an `op` field
/// (e.g. the unified `fs` tool called with `{"op":"write",…}`). This gates a
/// benign-named multi-op tool whose mutating mode would otherwise slip past the
/// name check.
///
/// Non-JSON or non-object arguments (and a missing/benign `op`) are not flagged.
#[must_use]
pub fn args_are_dangerous(arguments: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return false;
    };
    value
        .get("op")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|op| {
            let op = op.to_lowercase();
            DANGEROUS_OPS.iter().any(|verb| op == *verb)
        })
}

/// Whether any string value in the JSON `arguments` object contains a path that
/// escapes the workspace: an absolute path (starts with `/`) or a component that
/// traverses upward (`..`).
///
/// Non-JSON or non-object arguments are treated as safe (the name check or the
/// tool itself will reject them).
#[must_use]
pub fn args_escape_scope(arguments: &str) -> bool {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(arguments)
    else {
        return false;
    };
    string_values_recursive(&serde_json::Value::Object(map))
        .iter()
        .any(|s| path_escapes(s))
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

/// Whether a path string escapes the workspace root.
///
/// Flags absolute paths and paths whose components include `..`.
fn path_escapes(s: &str) -> bool {
    if s.starts_with('/') {
        return true;
    }
    // Check every `/`-separated component (covers both Unix and URL-style separators).
    s.split('/').any(|component| component == "..")
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

    #[test]
    fn dangerous_verbs_are_flagged() {
        for name in ["bash", "run_shell", "fs.delete", "exec_code", "rm-rf", "sudo_apt"] {
            assert!(is_dangerous(name), "{name:?} should require confirmation");
        }
    }

    #[test]
    fn ordinary_tools_are_not_flagged() {
        for name in ["web_search", "read_file", "fetch", "list_dir", "calculator"] {
            assert!(!is_dangerous(name), "{name:?} should not require confirmation");
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
        for args in [
            r#"{"op": "write", "path": "out.txt", "contents": "x"}"#,
            r#"{"op": "delete", "path": "out.txt"}"#,
            r#"{"op": "EXEC", "path": "x"}"#,
        ] {
            assert!(args_are_dangerous(args), "{args:?} should be flagged as a high-risk op");
        }
    }

    #[test]
    fn benign_op_arg_is_safe() {
        for args in [
            r#"{"op": "read", "path": "src/main.rs"}"#,
            r#"{"op": "grep", "path": "src/main.rs", "pattern": "fn"}"#,
            r#"{"path": "src/main.rs"}"#,
            "not json",
        ] {
            assert!(!args_are_dangerous(args), "{args:?} should not be flagged");
        }
    }

    #[test]
    fn traversal_in_path_arg_is_flagged() {
        for args in [
            r#"{"path": "../../etc/passwd"}"#,
            r#"{"path": "../sibling"}"#,
            r#"{"path": "a/b/../../secret"}"#,
            r#"{"file": "foo/../../../root"}"#,
        ] {
            assert!(args_escape_scope(args), "{args:?} should be flagged as scope escape");
        }
    }

    #[test]
    fn absolute_path_arg_is_flagged() {
        for args in [
            r#"{"path": "/etc/passwd"}"#,
            r#"{"path": "/home/user/.ssh/id_rsa"}"#,
            r#"{"args": ["/bin/sh", "-c", "whoami"]}"#,
        ] {
            assert!(args_escape_scope(args), "{args:?} should be flagged as absolute path");
        }
    }

    #[test]
    fn workspace_relative_paths_are_safe() {
        for args in [
            r#"{"path": "src/main.rs"}"#,
            r#"{"path": "subdir/file.txt"}"#,
            r#"{"path": "..hidden_file"}"#,
            r#"{"command": "echo", "args": ["hello"]}"#,
        ] {
            assert!(!args_escape_scope(args), "{args:?} should not be flagged");
        }
    }

    #[test]
    fn non_json_and_non_object_args_are_safe() {
        for args in ["", "not json", r#""a string""#, "42", "null"] {
            assert!(!args_escape_scope(args), "{args:?} should not be flagged");
        }
    }
}
