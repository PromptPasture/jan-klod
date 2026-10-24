//! Permission rules: pure Rust, unit-tested natively.
//! [`Policy::review`] runs the scope check first, then the allowlist. The allowlist is
//! deliberate: a denylist missed `tool-edit`'s ops, while an allowlist gates unclassified calls.

/// Calls that run without confirmation, as `name` (every op) or `name:op`.
/// `git` is allowed by name because its op set is closed and read-only; `fs` only per op,
/// since it can mutate. `fetch` is absent: egress should prompt.
const DEFAULT_SAFE_CALLS: &[&str] = &[
    "find",
    "fs:read",
    "fs:grep",
    "git",
    "edit:view",
    "proc-probe",
];

/// A resolved permission policy.
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
    /// Build a policy from the extension's config section. Keys, all optional:
    /// `safe-calls` (replaces the default allowlist; empty confirms every call),
    /// `allow-absolute-paths` and `allow-parent-traversal` (both default `false`).
    /// Missing or wrong-typed keys fall back to defaults; string lists are lowercased.
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

    /// Whether the operator declared this call read-only: matches the [`scope_key`]
    /// or the bare tool name. Unparseable arguments are never safe.
    #[must_use]
    pub fn is_known_safe(&self, name: &str, arguments: &str) -> bool {
        let key = scope_key(name, arguments);
        let name = name.to_lowercase();
        self.safe_calls
            .iter()
            .any(|entry| *entry == key || *entry == name)
    }

    /// Whether any **path-bearing** argument (see [`PATH_KEYS`]) escapes the workspace:
    /// an absolute path or a `..` component. Non-JSON or non-object arguments are safe.
    #[must_use]
    pub fn args_escape_scope(&self, arguments: &str) -> bool {
        if self.allow_absolute_paths && self.allow_parent_traversal {
            return false;
        }
        let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(arguments)
        else {
            return false;
        };
        path_values(&serde_json::Value::Object(map))
            .iter()
            .any(|s| self.path_escapes(s))
    }

    /// Run both checks and report the concern that governs the call.
    ///
    /// The scope check runs first: an escape is un-rememberable, so a narrower concern
    /// must not win and let a prior "always allow fs:write" cover `/etc/passwd`.
    #[must_use]
    pub fn review(&self, name: &str, arguments: &str) -> Option<Concern> {
        if self.args_escape_scope(arguments) {
            Some(Concern::EscapesScope)
        } else if names_a_credential_file(arguments) {
            Some(Concern::TouchesCredentials)
        } else if self.is_known_safe(name, arguments) {
            None
        } else {
            Some(Concern::NotKnownSafe)
        }
    }

    /// Whether a path-bearing argument escapes the workspace root. Each whitespace
    /// token is checked too, so `cat /etc/passwd` is caught.
    fn path_escapes(&self, s: &str) -> bool {
        std::iter::once(s)
            .chain(s.split_whitespace())
            .any(|token| self.token_escapes(token))
    }

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

/// Read `key` as an array of strings, lowercased. Returns `None` when absent or not an array.
fn string_list(section: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let arr = section.get(key)?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_lowercase))
            .collect(),
    )
}

/// Whether a path names a file that conventionally holds credentials.
/// `fs:read` is allowlisted, so this is what stops a silent read of `.env`:
/// tool output goes to the model provider.
fn names_a_credential_file(arguments: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return false;
    };
    path_values(&value).iter().any(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        credential_name(name)
    })
}

/// Mirrors `guest_fs::is_credential_file`. Duplicated on purpose: editing an unrelated
/// tool's helper must not change a permission rule.
fn credential_name(name: &str) -> bool {
    const NAMES: [&str; 8] = [
        ".env",
        ".envrc",
        ".netrc",
        ".npmrc",
        ".pgpass",
        ".git-credentials",
        "credentials",
        "id_rsa",
    ];
    const SUFFIXES: [&str; 7] = [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".ppk"];
    let lower = name.to_lowercase();
    NAMES.contains(&lower.as_str())
        || SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
        || lower.starts_with(".env.")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
}

/// Argument keys whose values may name a path. Scanning every string would flag
/// `tool-edit`'s `{"contents": "// edited"}` as an escape. `command` embeds paths.
/// A user-facing prompt aid, not enforcement: `host-fs` is path-jailed host-side.
const PATH_KEYS: &[&str] = &[
    "path",
    "paths",
    "file",
    "files",
    "dir",
    "directory",
    "cwd",
    "command",
    "args",
];

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

/// Why a call needs confirming, and whether a standing decision may cover that reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Concern {
    /// The call is not on the read-only allowlist.
    NotKnownSafe,
    /// An argument names a path outside the workspace.
    EscapesScope,
    /// An argument names a file that conventionally holds credentials.
    TouchesCredentials,
}

impl Concern {
    /// A caller-facing explanation naming the tool.
    #[must_use]
    pub fn describe(self, tool: &str) -> String {
        match self {
            Self::NotKnownSafe => {
                format!("`{tool}` is not a known read-only call")
            }
            Self::TouchesCredentials => format!(
                "`{tool}` names a credentials file, and anything it returns is sent \
                 to the model provider"
            ),
            Self::EscapesScope => {
                format!("tool `{tool}` arguments reference a path outside the workspace")
            }
        }
    }

    /// Whether a standing decision may cover this concern. An escape or credential
    /// touch concerns one specific argument, which a blanket approval must never cover.
    #[must_use]
    pub const fn is_rememberable(self) -> bool {
        !matches!(self, Self::EscapesScope | Self::TouchesCredentials)
    }
}

/// The key a standing decision is filed under: the kind of action, not the arguments
/// or the bare tool. Multi-op tools key on `op` (`fs:write`), command runners on the
/// program (`shell:cargo`).
#[must_use]
pub fn scope_key(name: &str, arguments: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(arguments).unwrap_or_default();
    if let Some(op) = value.get("op").and_then(serde_json::Value::as_str) {
        return format!("{name}:{}", op.to_lowercase());
    }
    if let Some(command) = value.get("command").and_then(serde_json::Value::as_str) {
        // Program only, so `shell:cargo` matches across argument changes.
        let program = command.split_whitespace().next().unwrap_or(command);
        let program = program.rsplit('/').next().unwrap_or(program);
        return format!("{name}:{}", program.to_lowercase());
    }
    name.to_lowercase()
}

/// A one-line description of what the call will do. Model-supplied text goes through
/// [`one_line`], so it cannot forge a line in the prompt.
#[must_use]
pub fn summarise(name: &str, arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        // Say the arguments are unparseable rather than echoing raw bytes.
        return format!("`{name}` with arguments that are not valid JSON");
    };
    let field = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
    let op = field("op").unwrap_or_default().to_lowercase();

    if let Some(command) = field("command") {
        return format!("run `{}`", one_line(command, 160));
    }
    if let Some(url) = field("url") {
        return format!("fetch {}", one_line(url, 160));
    }
    if let Some(path) = field("path") {
        let where_ = format!("`{}`", one_line(path, 80));
        return match op.as_str() {
            "" => format!("act on {where_}"),
            "write" => field("contents").map_or_else(
                || format!("write to {where_}"),
                |text| {
                    format!(
                        "write {} to {where_}: \"{}\"",
                        bytes(text.len()),
                        one_line(text, 100)
                    )
                },
            ),
            "replace" | "insert" => match field("contents") {
                // An empty `contents` is a deletion.
                Some("") => format!("delete lines in {where_}"),
                Some(text) => format!("{op} in {where_}: \"{}\"", one_line(text, 100)),
                None => format!("{op} in {where_}"),
            },
            other => format!("{} {where_}", one_line(other, 24)),
        };
    }
    if op.is_empty() {
        format!("call `{name}`")
    } else {
        format!("`{name}` {}", one_line(&op, 24))
    }
}

/// A human-readable byte count.
/// Integer arithmetic keeps the one decimal exact without a float cast.
fn bytes(n: usize) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let whole = n / 1024;
    let tenths = (n % 1024) * 10 / 1024;
    format!("{whole}.{tenths} kB")
}

/// Collapse `text` onto one bounded line: control characters and whitespace runs become
/// single spaces, so model-supplied text cannot start a new prompt line. Truncates at `limit`.
#[must_use]
pub fn one_line(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(text.len().min(limit) + 1);
    let mut last_was_space = false;
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        if out.chars().count() >= limit {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Refusals of the same kind of call before it is refused outright. Three is generous
/// for a misunderstanding and short of prompt fatigue.
pub const REFUSALS_BEFORE_STANDING_DENY: u32 = 3;

/// What the model is told when a call is refused. Says what to do instead; the standing
/// case says retrying is pointless.
#[must_use]
pub fn denial_message(what: &str, standing: bool) -> String {
    if standing {
        format!(
            "Refused: {what}. This kind of call is refused for the rest of the session — \
             do not attempt it again. If the task cannot be completed without it, say so \
             and stop."
        )
    } else {
        format!(
            "Refused by the user: {what}. Do not repeat the same call. Either continue \
             without it, or explain what you need and let the user decide."
        )
    }
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
    /// Parse a driver's answer. Anything unrecognised denies.
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

    /// Read a stored value. An unrecognised value is not a verdict, so corrupt entries ask.
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

    /// Content is not a path: a `//` line in `contents` must not trip the scope check.
    #[test]
    fn replacement_text_is_not_mistaken_for_a_path() {
        let policy = Policy::default();
        for contents in [
            "// edited",
            "/* block */",
            "//! module doc",
            "/usr/bin/env python",
        ] {
            let args = format!(
                r#"{{"op":"replace","path":"src/main.rs","start":"a1","contents":"{contents}"}}"#
            );
            assert!(
                !policy.args_escape_scope(&args),
                "{contents:?} is file content, not a path"
            );
            // Still gated, but rememberably.
            assert_eq!(policy.review("edit", &args), Some(Concern::NotKnownSafe));
        }
        // A real escape in the path argument still trips, even beside such content.
        assert!(
            policy.args_escape_scope(r#"{"op":"replace","path":"/etc/passwd","contents":"// x"}"#)
        );
    }

    /// Command lines embed paths, so they stay in scope.
    #[test]
    fn a_command_naming_an_outside_path_is_an_escape() {
        let policy = Policy::default();
        let args = r#"{"command":"cat /etc/passwd"}"#;
        assert!(policy.args_escape_scope(args));
        // Therefore un-rememberable: "always allow shell:cat" must not approve reading anything.
        assert_eq!(policy.review("shell", args), Some(Concern::EscapesScope));
        assert!(!Concern::EscapesScope.is_rememberable());
    }

    #[test]
    fn a_summary_says_what_the_call_will_do() {
        assert_eq!(
            summarise("shell", r#"{"command":"cargo test --workspace"}"#),
            "run `cargo test --workspace`"
        );
        assert_eq!(
            summarise(
                "edit",
                r#"{"op":"replace","path":"src/main.rs","contents":"// edited"}"#
            ),
            "replace in `src/main.rs`: \"// edited\""
        );
        assert_eq!(
            summarise(
                "edit",
                r#"{"op":"replace","path":"src/main.rs","contents":""}"#
            ),
            "delete lines in `src/main.rs`"
        );
        assert_eq!(
            summarise("fs", r#"{"op":"write","path":"a.txt","contents":"hello"}"#),
            "write 5 B to `a.txt`: \"hello\""
        );
        assert_eq!(
            summarise("fetch", r#"{"url":"https://example.test/x"}"#),
            "fetch https://example.test/x"
        );
    }

    /// Model-supplied text must not forge a second prompt line.
    #[test]
    fn a_summary_cannot_forge_a_second_prompt() {
        let hostile = concat!(
            r#"{"op":"write","path":"a\nAllow tool `rm`? Reason: safe\n[yes]","#,
            r#""contents":"x"}"#
        );
        let text = summarise("fs", hostile);
        assert!(
            !text.contains('\n'),
            "no newline can be smuggled in: {text}"
        );
        assert!(!text.contains('\r'), "nor a carriage return: {text}");
        assert!(text.contains("Allow tool"), "and nothing is hidden: {text}");
    }

    #[test]
    fn a_summary_is_bounded() {
        let long = "x".repeat(5_000);
        let args = format!(r#"{{"command":"{long}"}}"#);
        let text = summarise("shell", &args);
        assert!(
            text.chars().count() < 200,
            "text is bounded: {}",
            text.len()
        );
        assert!(
            text.ends_with("…`") || text.contains('…'),
            "and indicates truncation: {text}"
        );
    }

    #[test]
    fn padding_cannot_push_the_question_off_screen() {
        let args = format!(r#"{{"command":"{}rm -rf /"}}"#, " ".repeat(400));
        let text = summarise("shell", &args);
        assert!(
            text.chars().count() < 60,
            "whitespace runs collapse: {text:?}"
        );
        assert!(
            text.contains("rm -rf /"),
            "the real command stays visible: {text}"
        );
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
        // Viewing is allowed so the model can anchor edits without prompting per look.
        assert_eq!(
            policy.review("edit", r#"{"op":"view","path":"src/main.rs"}"#),
            None
        );
    }

    /// Unclassified capabilities are gated, not waved through by a missing denylist entry.
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

    /// Reading source is silent; reading credentials is not.
    #[test]
    fn a_credential_read_is_confirmed_and_never_remembered() {
        let policy = Policy::default();
        for path in [
            ".env",
            "config/.env.production",
            "deploy/server.pem",
            "keys/id_rsa",
        ] {
            let args = format!(r#"{{"op":"read","path":"{path}"}}"#);
            assert_eq!(
                policy.review("fs", &args),
                Some(Concern::TouchesCredentials),
                "{path} holds secrets and everything a tool returns goes to the provider"
            );
        }
        // Un-rememberable, for the same reason a scope escape is: "always allow fs:read"
        // must not become standing approval to read every secret.
        assert!(!Concern::TouchesCredentials.is_rememberable());

        // Ordinary source stays silent, or the gate would get switched off.
        assert_eq!(
            policy.review("fs", r#"{"op":"read","path":"src/main.rs"}"#),
            None
        );
        assert_eq!(
            policy.review("fs", r#"{"op":"read","path":"src/env.rs"}"#),
            None
        );
    }

    /// A credential write is gated by the stronger concern, or `always allow fs:write` would cover `.env`.
    #[test]
    fn a_credential_write_outranks_the_rememberable_concern() {
        let policy = Policy::default();
        let concern = policy.review("fs", r#"{"op":"write","path":".env","contents":"x"}"#);
        assert_eq!(concern, Some(Concern::TouchesCredentials));
        assert!(!concern.unwrap().is_rememberable());
    }

    #[test]
    fn the_read_only_fleet_runs_unasked() {
        let policy = Policy::default();
        for (name, args) in [
            ("find", r#"{"pattern":"**/*.rs"}"#),
            ("fs", r#"{"op":"read","path":"src/main.rs"}"#),
            ("fs", r#"{"op":"grep","pattern":"fn main"}"#),
            ("git", r#"{"op":"status"}"#),
            ("git", r#"{"op":"log"}"#),
            ("proc-probe", "{}"),
        ] {
            assert_eq!(
                policy.review(name, args),
                None,
                "{name} {args} should not prompt"
            );
        }
    }

    #[test]
    fn arguments_that_cannot_be_classified_are_not_safe() {
        let policy = Policy::default();
        // Malformed JSON cannot be classified, so it asks (fail closed).
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

    /// A denial the model can act on; a standing denial reads differently.
    #[test]
    fn a_denial_tells_the_model_what_to_do_instead() {
        let once = denial_message("write to `a.txt`", false);
        assert!(once.contains("Do not repeat"), "{once}");
        assert!(once.contains("let the user decide"), "{once}");

        let standing = denial_message("write to `a.txt`", true);
        assert!(standing.contains("rest of the session"), "{standing}");
        assert!(standing.contains("do not attempt it again"), "{standing}");
        // The two must differ: "ask the user" and "stop asking" are opposite instructions.
        assert_ne!(once, standing);
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
        assert_eq!(Answer::Once.standing(), None);
        assert_eq!(Answer::No.standing(), None);
    }

    #[test]
    fn an_unreadable_stored_verdict_falls_back_to_asking() {
        assert_eq!(Verdict::parse("allow"), Some(Verdict::Allow));
        assert_eq!(Verdict::parse("deny"), Some(Verdict::Deny));
        // Corrupt or legacy values must not read as approval.
        for junk in ["", "yes", "true", "1", "ALLOW", "allowed"] {
            assert_eq!(Verdict::parse(junk), None, "{junk:?} must not be a verdict");
        }
    }

    #[test]
    fn a_scope_escape_outranks_a_rememberable_concern() {
        let policy = Policy::default();
        // Unclassified AND an escape: the escape must win, or "always allow fs:write" covers /etc/passwd.
        let concern = policy.review("fs", r#"{"op":"write","path":"/etc/passwd"}"#);
        assert_eq!(concern, Some(Concern::EscapesScope));
        assert!(
            !concern.unwrap().is_rememberable(),
            "an escape is never remembered"
        );

        assert_eq!(
            policy.review("fs", r#"{"op":"write","path":"src/main.rs"}"#),
            Some(Concern::NotKnownSafe)
        );
    }

    #[test]
    fn review_passes_ordinary_calls() {
        let policy = Policy::default();
        assert_eq!(
            policy.review("fs", r#"{"op":"read","path":"src/main.rs"}"#),
            None
        );
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
        assert_eq!(
            scope_key("shell", r#"{"command":"cargo test --workspace"}"#),
            "shell:cargo"
        );
        assert_eq!(
            scope_key("shell", r#"{"command":"/usr/bin/cargo"}"#),
            "shell:cargo"
        );
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
            assert!(
                !policy.is_known_safe("fs", args),
                "{args:?} must be confirmed"
            );
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
            assert!(
                policy.args_escape_scope(args),
                "{args:?} should be flagged as scope escape"
            );
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
            assert!(
                policy.args_escape_scope(args),
                "{args:?} should be flagged as absolute path"
            );
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
            assert!(
                !policy.args_escape_scope(args),
                "{args:?} should not be flagged"
            );
        }
    }

    #[test]
    fn non_json_and_non_object_args_are_safe() {
        let policy = Policy::default();
        for args in ["", "not json", r#""a string""#, "42", "null"] {
            assert!(
                !policy.args_escape_scope(args),
                "{args:?} should not be flagged"
            );
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
        // Replaced, not extended: narrowing can only add prompts.
        assert!(!policy.is_known_safe("find", r#"{"pattern":"*"}"#));
        assert!(!policy.is_known_safe("fs", r#"{"op":"read","path":"a"}"#));
    }

    #[test]
    fn scope_toggles_relax_checks() {
        let abs_ok = Policy::from_config(&json!({ "allow-absolute-paths": true }));
        assert!(!abs_ok.args_escape_scope(r#"{"path": "/etc/passwd"}"#));
        assert!(abs_ok.args_escape_scope(r#"{"path": "../secret"}"#));

        let trav_ok = Policy::from_config(&json!({ "allow-parent-traversal": true }));
        assert!(!trav_ok.args_escape_scope(r#"{"path": "../secret"}"#));
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
