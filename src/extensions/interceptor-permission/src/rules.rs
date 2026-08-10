//! Permission rules — pure Rust, unit-tested natively.
//!
//! Two checks gate a tool call, both driven by a [`Policy`] the component builds
//! from its `config.yaml` section:
//!
//! 1. **Scope check** ([`Policy::args_escape_scope`]): any string argument
//!    contains a path that would leave the workspace — an absolute path (`/…`) or
//!    a component that traverses upward (`..`).
//!
//! 2. **Allowlist check** ([`Policy::is_known_safe`]): the call is one of the
//!    read-only operations the operator has named. Anything else is confirmed.
//!
//! ## Why an allowlist
//!
//! This was a **denylist** of high-risk verbs: tool names containing `shell`,
//! `write`, `exec`…, plus `op` values like `write` and `delete`. It reads as
//! reasonable and it is the wrong shape, because the list can only name the verbs
//! someone thought of. `tool-edit` shipped with the ops `view`, `replace` and
//! `insert`. None of those words is `write`, and `edit` is not `shell` — so the
//! tool whose entire purpose is modifying files in place went through the gate
//! **without ever asking**, from the commit that added it. Nothing failed,
//! because a denylist that misses something is indistinguishable from one that
//! has nothing to catch.
//!
//! A fail-closed boundary cannot be spelled as "these things are dangerous". It
//! has to be "these things are safe" — then a capability nobody has classified is
//! gated by construction, which is what you want from the *unknown* ones
//! specifically. The cost is real and worth naming: add a tool and it prompts
//! until someone puts it on the list. That is the correct direction for the
//! mistake to point.

/// Calls that run without confirmation: read-only operations of the shipped
/// fleet, as `name` (every op) or `name:op`.
///
/// Deliberately short and deliberately boring. `git` is here as a bare name
/// because the tool exposes a closed, read-only op set and cannot express a
/// write; `fs` is not, because it can. `fetch` is absent on purpose — it is
/// egress, and a coding agent reaching the network is worth one question.
const DEFAULT_SAFE_CALLS: &[&str] = &[
    "find",
    "fs:read",
    "fs:grep",
    "git",
    "edit:view",
    "proc-probe",
];

/// A resolved permission policy: the three checks read from these fields rather
/// than module constants, so the same rules serve both the built-in defaults and
/// a config-driven override.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Calls that proceed unasked, as `name` or `name:op` (lowercase).
    safe_calls: Vec<String>,
    /// When true, absolute-path arguments do not trip the scope check.
    allow_absolute_paths: bool,
    /// When true, `..` traversal in path arguments does not trip the scope check.
    allow_parent_traversal: bool,
}

impl Default for Policy {
    /// The built-in policy: the read-only fleet allowed, both scope checks on.
    fn default() -> Self {
        Self {
            safe_calls: DEFAULT_SAFE_CALLS.iter().map(|s| (*s).to_owned()).collect(),
            allow_absolute_paths: false,
            allow_parent_traversal: false,
        }
    }
}

impl Policy {
    /// Build a policy from an extension config section (the JSON object served by
    /// `host-config::all`). Recognised keys, each optional:
    ///
    /// - `safe-calls`: array of `name` / `name:op` — **replaces** the default
    ///   allowlist. An empty array confirms every call.
    /// - `allow-absolute-paths`: bool — default `false`.
    /// - `allow-parent-traversal`: bool — default `false`.
    ///
    /// A missing or wrong-typed key falls back to the default. String lists are
    /// lower-cased so matching stays case-insensitive.
    #[must_use]
    pub fn from_config(section: &serde_json::Value) -> Self {
        let default = Self::default();
        Self {
            safe_calls: string_list(section, "safe-calls").unwrap_or(default.safe_calls),
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

    /// Whether this call is one the operator has declared read-only.
    ///
    /// Matches the [`scope_key`] (`fs:read`, `edit:view`, `shell:cargo`) or the
    /// bare tool name, so an entry can allow a whole tool or one of its ops. A
    /// tool with a closed read-only op set — `git` — is allowed by name; one that
    /// can also mutate — `fs` — is allowed only per op.
    ///
    /// Unparseable arguments cannot be classified, so they are not safe. That is
    /// the fail-closed direction: the previous checks returned `false` ("not
    /// dangerous") on malformed JSON.
    #[must_use]
    pub fn is_known_safe(&self, name: &str, arguments: &str) -> bool {
        let key = scope_key(name, arguments);
        let name = name.to_lowercase();
        self.safe_calls.iter().any(|entry| *entry == key || *entry == name)
    }

    /// Whether any **path-bearing** argument (see [`PATH_KEYS`]) escapes the
    /// workspace: an absolute path (starts with `/`) or a component that traverses
    /// upward (`..`). Either check can be disabled via policy toggles.
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
        path_values(&serde_json::Value::Object(map)).iter().any(|s| self.path_escapes(s))
    }

    /// Run both checks and report the concern that governs the call.
    ///
    /// **The scope check runs first, and that ordering is the security property.**
    /// A concern is what a standing "always allow" gets filed against, and only
    /// [`Concern::EscapesScope`] is un-rememberable — so if a path escape were
    /// reported second, `{"op":"write","path":"/etc/passwd"}` would surface as the
    /// *rememberable* `NotKnownSafe` and a prior "always allow fs:write" would
    /// wave it through. Escapes dominate; the narrower concern only shows when
    /// the arguments stay inside the workspace.
    #[must_use]
    pub fn review(&self, name: &str, arguments: &str) -> Option<Concern> {
        if self.args_escape_scope(arguments) {
            Some(Concern::EscapesScope)
        } else if self.is_known_safe(name, arguments) {
            None
        } else {
            Some(Concern::NotKnownSafe)
        }
    }

    /// Whether a path-bearing argument escapes the workspace root.
    ///
    /// Checked per whitespace-separated token, not just on the whole string. A
    /// `command` value is one string containing several: `cat /etc/passwd` does
    /// not *start* with a slash, so testing the string as a whole missed it
    /// entirely and the call surfaced as the rememberable `shell:cat` — one
    /// "always" away from standing approval to read any file on the machine.
    fn path_escapes(&self, s: &str) -> bool {
        std::iter::once(s).chain(s.split_whitespace()).any(|token| self.token_escapes(token))
    }

    /// The escape test for a single path-like token, honouring the toggles.
    fn token_escapes(&self, token: &str) -> bool {
        if !self.allow_absolute_paths && token.starts_with('/') {
            return true;
        }
        if !self.allow_parent_traversal && token.split('/').any(|component| component == "..") {
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

/// Argument keys whose values may name a location on disk.
///
/// The scope check used to scan **every** string in the arguments, which is
/// wrong once a tool carries content as well as paths: `tool-edit`'s
/// `{"contents": "// edited"}` starts with a slash, so replacing a line with a
/// Rust, C, Go or JavaScript comment was reported as "references a path outside
/// the workspace" — and scope escapes are deliberately un-rememberable, so the
/// user could not even silence it. A gate that cries wolf about a code comment
/// teaches people to click through it, which costs more than the narrow miss
/// this trades for.
///
/// `command` is here because a command line embeds paths (`cat /etc/passwd`)
/// and, without it, that call would surface as the *rememberable* `shell:cat`.
///
/// The narrowing is safe because this check is not the enforcement. `host-fs` is
/// path-jailed host-side and refuses an escape whatever the interceptor decided
/// (`host_fs::tests::escapes_are_denied`). This exists to put the escape in front
/// of the user in the words they need, before the tool runs.
const PATH_KEYS: &[&str] =
    &["path", "paths", "file", "files", "dir", "directory", "cwd", "command", "args"];

/// Collect string values that sit under a path-bearing key, at any depth.
fn path_values(value: &serde_json::Value) -> Vec<&str> {
    fn walk<'a>(value: &'a serde_json::Value, under_path_key: bool, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(s) => {
                if under_path_key {
                    out.push(s.as_str());
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    walk(item, under_path_key, out);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    let is_path = PATH_KEYS.contains(&key.to_lowercase().as_str());
                    walk(item, under_path_key || is_path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(value, false, &mut out);
    out
}

/// Why a call needs confirming — and, crucially, whether that reason is one a
/// standing decision may cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concern {
    /// The call is not on the read-only allowlist.
    NotKnownSafe,
    /// An argument names a path outside the workspace.
    EscapesScope,
}

impl Concern {
    /// A caller-facing explanation naming the tool.
    #[must_use]
    pub fn describe(self, tool: &str) -> String {
        match self {
            Self::NotKnownSafe => {
                format!("`{tool}` is not a known read-only call")
            }
            Self::EscapesScope => {
                format!("tool `{tool}` arguments reference a path outside the workspace")
            }
        }
    }

    /// Whether "don't ask again" may apply to this concern.
    ///
    /// **A scope escape is never remembered.** Standing decisions are about a
    /// *kind of action* ("yes, this agent may write files"), and a path leaving
    /// the workspace is about a *specific argument* — the one thing a blanket
    /// approval must not silently cover. Approving one write to `src/` must never
    /// become approval for a write to `/etc/passwd`.
    #[must_use]
    pub const fn is_rememberable(self) -> bool {
        !matches!(self, Self::EscapesScope)
    }
}

/// The key a standing decision is filed under: the *kind* of action, not the
/// exact arguments (which never repeat) and not the bare tool (too broad).
///
/// A multi-op tool keys on its `op` (`fs:write`), a command runner on the program
/// it runs (`shell:cargo`), anything else on its name. So approving `cargo` for
/// the session does not also approve `curl`.
#[must_use]
pub fn scope_key(name: &str, arguments: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(arguments).unwrap_or_default();
    if let Some(op) = value.get("op").and_then(serde_json::Value::as_str) {
        return format!("{name}:{}", op.to_lowercase());
    }
    if let Some(command) = value.get("command").and_then(serde_json::Value::as_str) {
        // The program, not the whole command line: `shell:cargo`, never
        // `shell:cargo test --workspace`, which would never match twice.
        let program = command.split_whitespace().next().unwrap_or(command);
        let program = program.rsplit('/').next().unwrap_or(program);
        return format!("{name}:{}", program.to_lowercase());
    }
    name.to_lowercase()
}

/// What the driver answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Allow this one call.
    Once,
    /// Allow this kind of call for the rest of the run.
    Always,
    /// Deny this one call.
    No,
    /// Deny this kind of call for the rest of the run.
    Never,
}

impl Answer {
    /// Parse a driver's answer. **Anything unrecognised denies** — the safe
    /// default for a permission gate, including the empty answer a disconnected
    /// or confused driver sends.
    #[must_use]
    pub fn parse(answer: &str) -> Self {
        match answer.trim().to_lowercase().as_str() {
            "y" | "yes" | "allow" | "approve" | "ok" => Self::Once,
            "a" | "always" | "yes-always" | "allow-always" => Self::Always,
            "never" | "no-never" | "deny-always" => Self::Never,
            _ => Self::No,
        }
    }

    /// Whether the call may run.
    #[must_use]
    pub const fn approves(self) -> bool {
        matches!(self, Self::Once | Self::Always)
    }

    /// The verdict to remember for this scope, if any.
    #[must_use]
    pub const fn standing(self) -> Option<Verdict> {
        match self {
            Self::Always => Some(Verdict::Allow),
            Self::Never => Some(Verdict::Deny),
            Self::Once | Self::No => None,
        }
    }
}

/// A remembered decision for a scope key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Run without asking again.
    Allow,
    /// Block without asking again.
    Deny,
}

impl Verdict {
    /// The stored representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    /// Read a stored value back. **An unrecognised value is not a verdict**, so a
    /// corrupted or half-written entry falls back to asking rather than to
    /// allowing.
    #[must_use]
    pub fn parse(stored: &str) -> Option<Self> {
        match stored.trim() {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The gap that motivated inverting the policy.
    ///
    /// Under the old denylist every one of these went through unasked: `edit`
    /// contains no high-risk verb, and `replace`/`insert` are not `write`. The
    /// tool whose entire purpose is modifying files in place was ungated from the
    /// commit that added it.
    /// Content is not a path.
    ///
    /// `{"contents":"// edited"}` starts with a slash. Scanning every string made
    /// that a scope escape, so replacing a line with a comment in Rust, C, Go or
    /// JavaScript raised "references a path outside the workspace" — and an escape
    /// is un-rememberable by design, so it could not be silenced either.
    #[test]
    fn replacement_text_is_not_mistaken_for_a_path() {
        let policy = Policy::default();
        for contents in ["// edited", "/* block */", "//! module doc", "/usr/bin/env python"] {
            let args = format!(
                r#"{{"op":"replace","path":"src/main.rs","start":"a1","contents":"{contents}"}}"#
            );
            assert!(
                !policy.args_escape_scope(&args),
                "{contents:?} is file content, not a path"
            );
            // Still gated — just for the right reason, and rememberably.
            assert_eq!(policy.review("edit", &args), Some(Concern::NotKnownSafe));
        }
        // A real escape in the path argument still trips, even beside such content.
        assert!(policy.args_escape_scope(
            r#"{"op":"replace","path":"/etc/passwd","contents":"// x"}"#
        ));
    }

    /// A command line embeds paths, so it stays in scope.
    #[test]
    fn a_command_naming_an_outside_path_is_an_escape() {
        let policy = Policy::default();
        let args = r#"{"command":"cat /etc/passwd"}"#;
        assert!(policy.args_escape_scope(args));
        // And therefore un-rememberable: "always allow shell:cat" must not become
        // standing approval for reading anything on the machine.
        assert_eq!(policy.review("shell", args), Some(Concern::EscapesScope));
        assert!(!Concern::EscapesScope.is_rememberable());
    }

    #[test]
    fn the_edit_tool_is_gated() {
        let policy = Policy::default();
        for args in [
            r#"{"op":"replace","path":"src/main.rs","start":"a1b2","contents":"x"}"#,
            r#"{"op":"insert","path":"src/main.rs","after":"a1b2","contents":"x"}"#,
        ] {
            assert_eq!(
                policy.review("edit", args),
                Some(Concern::NotKnownSafe),
                "{args:?} modifies a file and must be confirmed"
            );
        }
        // Viewing is the read half, and is allowed so the model can anchor an edit
        // without a prompt for every look.
        assert_eq!(policy.review("edit", r#"{"op":"view","path":"src/main.rs"}"#), None);
    }

    /// The property the allowlist exists for: a capability nobody classified is
    /// gated, rather than waved through because no denylist entry matched it.
    #[test]
    fn a_tool_nobody_has_classified_is_confirmed() {
        let policy = Policy::default();
        for (name, args) in [
            ("deploy", "{}"),
            ("send_email", r#"{"to":"ops@example.test"}"#),
            ("fs", r#"{"op":"chmod","path":"x"}"#),
            ("some-future-tool", r#"{"op":"harmless-sounding"}"#),
        ] {
            assert_eq!(
                policy.review(name, args),
                Some(Concern::NotKnownSafe),
                "{name} {args} is unclassified and must be confirmed"
            );
        }
    }

    /// Egress is not read-only, even though it does not touch the workspace.
    #[test]
    fn fetching_a_url_is_confirmed() {
        let policy = Policy::default();
        assert_eq!(
            policy.review("fetch", r#"{"url":"https://example.test/x"}"#),
            Some(Concern::NotKnownSafe)
        );
    }

    #[test]
    fn the_read_only_fleet_runs_unasked() {
        let policy = Policy::default();
        for (name, args) in [
            ("find", r#"{"pattern":"**/*.rs"}"#),
            ("fs", r#"{"op":"read","path":"src/main.rs"}"#),
            ("fs", r#"{"op":"grep","pattern":"fn main"}"#),
            // `git` is allowed by bare name: its op set is closed and read-only,
            // so there is no write for a new op to smuggle in.
            ("git", r#"{"op":"status"}"#),
            ("git", r#"{"op":"log"}"#),
            ("proc-probe", "{}"),
        ] {
            assert_eq!(policy.review(name, args), None, "{name} {args} should not prompt");
        }
    }

    #[test]
    fn arguments_that_cannot_be_classified_are_not_safe() {
        let policy = Policy::default();
        // The old checks answered "not dangerous" for malformed JSON. An
        // allowlist cannot classify it either — but the fail-closed reading of
        // "cannot classify" is to ask.
        assert_eq!(policy.review("fs", "not json"), Some(Concern::NotKnownSafe));
    }

    #[test]
    fn an_operator_can_widen_or_narrow_the_allowlist() {
        let wide = Policy::from_config(&json!({ "safe-calls": ["fs", "shell:cargo"] }));
        assert_eq!(wide.review("fs", r#"{"op":"write","path":"a"}"#), None);
        assert_eq!(wide.review("shell", r#"{"command":"cargo test"}"#), None);
        assert_eq!(
            wide.review("shell", r#"{"command":"curl evil.test"}"#),
            Some(Concern::NotKnownSafe),
            "widening for cargo must not widen for curl"
        );

        // An empty list confirms everything — the strictest setting, and reachable.
        let strict = Policy::from_config(&json!({ "safe-calls": [] }));
        assert_eq!(strict.review("find", "{}"), Some(Concern::NotKnownSafe));
    }

    #[test]
    fn only_explicit_yes_approves() {
        for yes in ["y", "Yes", " ALLOW ", "approve", "ok"] {
            assert!(Answer::parse(yes).approves(), "{yes:?} should approve");
        }
        for no in ["", "n", "no", "nope", "cancel", "later", "maybe"] {
            assert!(!Answer::parse(no).approves(), "{no:?} should deny");
        }
    }

    #[test]
    fn standing_answers_are_parsed_and_carry_a_verdict() {
        assert_eq!(Answer::parse("always"), Answer::Always);
        assert_eq!(Answer::parse(" ALWAYS "), Answer::Always);
        assert_eq!(Answer::parse("never"), Answer::Never);

        assert!(Answer::Always.approves());
        assert!(!Answer::Never.approves());
        assert_eq!(Answer::Always.standing(), Some(Verdict::Allow));
        assert_eq!(Answer::Never.standing(), Some(Verdict::Deny));
        // A one-off answer leaves nothing behind.
        assert_eq!(Answer::Once.standing(), None);
        assert_eq!(Answer::No.standing(), None);
    }

    #[test]
    fn an_unreadable_stored_verdict_falls_back_to_asking() {
        assert_eq!(Verdict::parse("allow"), Some(Verdict::Allow));
        assert_eq!(Verdict::parse("deny"), Some(Verdict::Deny));
        // Corrupt/half-written/legacy values must not read as approval.
        for junk in ["", "yes", "true", "1", "ALLOW", "allowed"] {
            assert_eq!(Verdict::parse(junk), None, "{junk:?} must not be a verdict");
        }
    }

    #[test]
    fn a_scope_escape_outranks_a_rememberable_concern() {
        let policy = Policy::default();
        // Both an unclassified call AND a path escape. If the former were
        // reported, a prior "always allow fs:write" would wave through a write to
        // /etc/passwd.
        let concern = policy.review("fs", r#"{"op":"write","path":"/etc/passwd"}"#);
        assert_eq!(concern, Some(Concern::EscapesScope));
        assert!(!concern.unwrap().is_rememberable(), "an escape is never remembered");

        // In-workspace, the narrower rememberable concern surfaces as usual.
        assert_eq!(
            policy.review("fs", r#"{"op":"write","path":"src/main.rs"}"#),
            Some(Concern::NotKnownSafe)
        );
    }

    #[test]
    fn review_passes_ordinary_calls() {
        let policy = Policy::default();
        assert_eq!(policy.review("fs", r#"{"op":"read","path":"src/main.rs"}"#), None);
        assert_eq!(policy.review("find", r#"{"pattern":"**/*.rs"}"#), None);
    }

    #[test]
    fn a_standing_decision_is_keyed_by_the_kind_of_action() {
        // Multi-op tools key on the op, so approving reads never approves writes.
        assert_eq!(scope_key("fs", r#"{"op":"write","path":"a"}"#), "fs:write");
        assert_ne!(
            scope_key("fs", r#"{"op":"write","path":"a"}"#),
            scope_key("fs", r#"{"op":"delete","path":"a"}"#)
        );
        // The same kind of action keys the same regardless of its arguments —
        // otherwise "always" would never match a second time.
        assert_eq!(
            scope_key("fs", r#"{"op":"write","path":"a"}"#),
            scope_key("fs", r#"{"op":"write","path":"b","contents":"x"}"#)
        );
    }

    #[test]
    fn a_command_runner_keys_on_the_program_not_the_command_line() {
        assert_eq!(scope_key("shell", r#"{"command":"cargo test --workspace"}"#), "shell:cargo");
        assert_eq!(scope_key("shell", r#"{"command":"/usr/bin/cargo"}"#), "shell:cargo");
        // Approving `cargo` for the run must not also approve `curl`.
        assert_ne!(
            scope_key("shell", r#"{"command":"cargo"}"#),
            scope_key("shell", r#"{"command":"curl"}"#)
        );
    }

    #[test]
    fn a_tool_without_a_discriminator_keys_on_its_name() {
        assert_eq!(scope_key("delete_all", "{}"), "delete_all");
        assert_eq!(scope_key("Delete_All", "not json"), "delete_all");
    }

    #[test]
    fn a_mutating_op_on_an_allowed_tool_is_still_confirmed() {
        let policy = Policy::default();
        // `fs:read` being safe must not make `fs` safe.
        for args in [
            r#"{"op": "write", "path": "out.txt", "contents": "x"}"#,
            r#"{"op": "WRITE", "path": "out.txt"}"#,
        ] {
            assert!(!policy.is_known_safe("fs", args), "{args:?} must be confirmed");
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
    fn config_replaces_the_allowlist_rather_than_extending_it() {
        let policy = Policy::from_config(&json!({ "safe-calls": ["ping"] }));
        assert!(policy.is_known_safe("ping", "{}"));
        // The defaults are gone — replaced, not extended. Narrowing this way is
        // safe in the direction that matters: it can only add prompts.
        assert!(!policy.is_known_safe("find", r#"{"pattern":"*"}"#));
        assert!(!policy.is_known_safe("fs", r#"{"op":"read","path":"a"}"#));
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
            &json!({ "safe-calls": "not-an-array", "allow-absolute-paths": "yes" }),
        );
        assert_eq!(policy, Policy::default());
    }
}
