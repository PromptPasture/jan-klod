//! The single permission rule — pure Rust, unit-tested natively.
//!
//! Thin v1: a tool call is "dangerous" when its name contains one of a small set
//! of high-risk verbs. This is deliberately conservative (name-based, no argument
//! parsing) — the point is to prove the `ask` round-trip and the fail-closed
//! policy, not to be a complete policy engine. A config-driven pattern list is a
//! later refinement.

/// High-risk substrings: a tool whose name contains any of these prompts a
/// confirmation. Kept lowercase; matching lower-cases the tool name first.
const DANGEROUS: &[&str] = &[
    "bash", "shell", "exec", "eval", "rm", "delete", "remove", "write", "kill", "sudo",
];

/// Whether a tool call to `name` should be confirmed before running.
#[must_use]
pub fn is_dangerous(name: &str) -> bool {
    let name = name.to_lowercase();
    DANGEROUS.iter().any(|verb| name.contains(verb))
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
}
