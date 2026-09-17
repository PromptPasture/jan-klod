//! Guardrail rules — pure Rust, unit-tested natively.
//!
//! A rule is **data**: a pattern, where it applies, and what to do when it
//! matches. Nothing here executes anything the operator wrote; the only thing
//! read from `config.yaml` is text, and the only thing done with it is matching.
//!
//! ## Why the engine matters
//!
//! Matching runs on the path of every tool call, so a pattern that can be made
//! to backtrack is a denial-of-service surface sitting on the hot path — the
//! opposite of the point. The `regex` crate matches in linear time and cannot
//! backtrack, which is what rules out lookaround and backreferences. That limit
//! is the guarantee, not a shortcoming to work around.
//!
//! ## A malformed rule is an error, not an absence
//!
//! An unparseable pattern makes the whole rule set invalid. The component then
//! returns `interceptor-error::internal`, and host policy takes it from there:
//! **fail closed at `tool-call`**, log and proceed elsewhere (`wit/interceptor.wit`).
//! Skipping the bad rule and carrying on would turn an operator's typo into a
//! silently missing guardrail, which is the failure nobody notices.

use regex::Regex;

/// What a matching rule does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Refuse the action and surface the rule's reason.
    Block,
    /// Put it to the user, defaulting to refusal.
    Ask,
}

impl Act {
    /// Parse the `decision` key. Absent means [`Act::Block`]: a rule an operator
    /// bothered to write is a rule they want enforced, and a typo'd decision
    /// must not quietly downgrade to the weaker one.
    fn parse(raw: Option<&str>) -> Result<Self, RuleError> {
        match raw {
            None | Some("block") => Ok(Self::Block),
            Some("ask") => Ok(Self::Ask),
            Some(other) => Err(RuleError::Decision(other.to_owned())),
        }
    }
}

/// Why a rule set could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleError {
    /// The pattern is not a valid regular expression.
    Pattern(String),
    /// A rule carried no `pattern` key.
    MissingPattern,
    /// The `decision` key held something other than `block` or `ask`.
    Decision(String),
    /// A text rule's `decision` key held something other than `replace` or `block`.
    TextDecision(String),
}

impl RuleError {
    /// One line, naming the offending value, for the log and the host.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Pattern(pattern) => format!("`{pattern}` is not a valid pattern"),
            Self::MissingPattern => "a rule has no `pattern`".to_owned(),
            Self::Decision(raw) => {
                format!("`{raw}` is not a decision; use `block` or `ask`")
            }
            Self::TextDecision(raw) => {
                format!("`{raw}` is not a decision for a `redact` rule; use `replace` or `block`")
            }
        }
    }
}

/// One rule over a tool call's arguments.
#[derive(Debug)]
pub struct DenyRule {
    pattern: Regex,
    /// The tool this rule applies to, or every tool when absent.
    tool: Option<String>,
    /// Shown to the user when the rule fires.
    reason: String,
    act: Act,
}

/// What a rule decided about one subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// The rule's own words, surfaced to whoever is refused or asked.
    pub reason: String,
    /// Block or ask.
    pub act: Act,
}

/// What a matching text rule does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAct {
    /// Rewrite the match out of the text and let the turn continue.
    Replace,
    /// Refuse: the text does not go where it was headed.
    Block,
}

impl TextAct {
    /// Parse the `decision` key of a text rule. Absent means [`TextAct::Replace`]
    /// — a redaction list is for redacting, and the loud option should be the
    /// one an operator has to write down.
    fn parse(raw: Option<&str>) -> Result<Self, RuleError> {
        match raw {
            None | Some("replace") => Ok(Self::Replace),
            Some("block") => Ok(Self::Block),
            Some(other) => Err(RuleError::TextDecision(other.to_owned())),
        }
    }
}

/// What the operator's replacement text is when they name none.
const DEFAULT_REPLACEMENT: &str = "[redacted]";

/// One rule over text — model output, the final answer, or a message on its
/// way to the provider.
#[derive(Debug)]
pub struct TextRule {
    pattern: Regex,
    /// What a match becomes. Unused when the rule blocks.
    with: String,
    /// Shown when the rule refuses.
    reason: String,
    act: TextAct,
}

/// What the text rules did to one piece of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextVerdict {
    /// Every match rewritten; carries the new text.
    Redacted(String),
    /// A rule refused the text outright, with its reason.
    Blocked(String),
}

/// The configured rule set. Empty is the normal state: a guardrail nobody
/// configured must not change what the loop does.
#[derive(Debug, Default)]
pub struct Rules {
    deny_tool_arguments: Vec<DenyRule>,
    redact: Vec<TextRule>,
}

impl Rules {
    /// Build the rule set from this extension's `config.yaml` section, as the
    /// JSON object `host-config::all` returns.
    ///
    /// Recognised keys, both optional:
    ///
    /// ```yaml
    /// deny-tool-arguments:
    ///   - pattern: "rm +-rf"        # required
    ///     tool: shell               # optional; every tool when absent
    ///     reason: "recursive delete"# optional
    ///     decision: block           # optional; `block` or `ask`, default block
    /// redact:
    ///   - pattern: "sk-[A-Za-z0-9]{16,}"  # required
    ///     with: "[redacted]"              # optional, this is the default
    ///     reason: "an API key"            # optional; shown when it blocks
    ///     decision: replace               # optional; `replace` or `block`
    /// ```
    ///
    /// A section that is absent, null, or carries no rules yields an empty set.
    ///
    /// # Errors
    /// Returns the first malformed rule. One bad rule invalidates the set —
    /// see the module doc for why.
    pub fn from_config(section: &serde_json::Value) -> Result<Self, RuleError> {
        Ok(Self {
            deny_tool_arguments: Self::tool_rules(section)?,
            redact: Self::text_rules(section)?,
        })
    }

    /// The `deny-tool-arguments` list, or empty when the key is absent.
    fn tool_rules(section: &serde_json::Value) -> Result<Vec<DenyRule>, RuleError> {
        let Some(raw) = section
            .get("deny-tool-arguments")
            .and_then(|v| v.as_array())
        else {
            return Ok(Vec::new());
        };
        let mut rules = Vec::with_capacity(raw.len());
        for rule in raw {
            let (pattern, compiled) = compile(rule)?;
            rules.push(DenyRule {
                pattern: compiled,
                tool: rule
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                reason: reason_of(rule, pattern),
                act: Act::parse(rule.get("decision").and_then(serde_json::Value::as_str))?,
            });
        }
        Ok(rules)
    }

    /// The `redact` list, or empty when the key is absent.
    fn text_rules(section: &serde_json::Value) -> Result<Vec<TextRule>, RuleError> {
        let Some(raw) = section.get("redact").and_then(|v| v.as_array()) else {
            return Ok(Vec::new());
        };
        let mut rules = Vec::with_capacity(raw.len());
        for rule in raw {
            let (pattern, compiled) = compile(rule)?;
            rules.push(TextRule {
                pattern: compiled,
                with: rule
                    .get("with")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(DEFAULT_REPLACEMENT)
                    .to_owned(),
                reason: reason_of(rule, pattern),
                act: TextAct::parse(rule.get("decision").and_then(serde_json::Value::as_str))?,
            });
        }
        Ok(rules)
    }

    /// The first rule that fires against this call's arguments, if any.
    ///
    /// First rather than most severe: rules are ordered by the operator, and an
    /// order they wrote is more predictable than a precedence we invent.
    #[must_use]
    pub fn review_tool_call(&self, tool: &str, arguments: &str) -> Option<Verdict> {
        self.deny_tool_arguments
            .iter()
            .find(|rule| {
                rule.tool.as_deref().is_none_or(|named| named == tool)
                    && rule.pattern.is_match(arguments)
            })
            .map(|rule| Verdict {
                reason: rule.reason.clone(),
                act: rule.act,
            })
    }

    /// Apply the `redact` rules to one piece of text.
    ///
    /// `None` means no rule touched it, which is the answer that leaves the
    /// turn exactly as it was. A blocking rule wins over redaction and is
    /// checked first: once an operator has said this text may not pass, a
    /// partial rewrite of it is not the outcome they asked for.
    #[must_use]
    pub fn review_text(&self, text: &str) -> Option<TextVerdict> {
        if let Some(refusal) = self
            .redact
            .iter()
            .find(|rule| rule.act == TextAct::Block && rule.pattern.is_match(text))
        {
            return Some(TextVerdict::Blocked(refusal.reason.clone()));
        }
        let mut redacted = std::borrow::Cow::Borrowed(text);
        for rule in self.redact.iter().filter(|r| r.act == TextAct::Replace) {
            if rule.pattern.is_match(&redacted) {
                redacted = std::borrow::Cow::Owned(
                    rule.pattern.replace_all(&redacted, &rule.with).into_owned(),
                );
            }
        }
        match redacted {
            std::borrow::Cow::Borrowed(_) => None,
            std::borrow::Cow::Owned(text) => Some(TextVerdict::Redacted(text)),
        }
    }
}

/// The `pattern` key of a rule, compiled. Returns both so the caller can use
/// the source text in a default reason.
fn compile(rule: &serde_json::Value) -> Result<(&str, Regex), RuleError> {
    let pattern = rule
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .ok_or(RuleError::MissingPattern)?;
    let compiled = Regex::new(pattern).map_err(|_| RuleError::Pattern(pattern.to_owned()))?;
    Ok((pattern, compiled))
}

/// The rule's `reason`, or one naming the pattern — so a rule written without
/// a reason still tells the user what fired.
fn reason_of(rule: &serde_json::Value, pattern: &str) -> String {
    rule.get("reason")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || format!("matches the guardrail `{pattern}`"),
            ToOwned::to_owned,
        )
}

#[cfg(test)]
mod tests {
    use super::{Act, RuleError, Rules, TextVerdict};

    fn config(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("the test's own JSON parses")
    }

    #[test]
    fn no_configuration_means_no_opinion() {
        let rules = Rules::from_config(&config("{}")).expect("an empty section is valid");
        assert!(rules
            .review_tool_call("shell", r#"{"command":"rm -rf /"}"#)
            .is_none());
    }

    #[test]
    fn an_empty_rule_list_is_not_an_error() {
        let rules =
            Rules::from_config(&config(r#"{"deny-tool-arguments":[]}"#)).expect("valid, if idle");
        assert!(rules.review_tool_call("shell", "anything").is_none());
    }

    #[test]
    fn a_matching_argument_is_blocked_with_the_rules_own_reason() {
        let rules = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"rm +-rf","reason":"recursive delete"}]}"#,
        ))
        .expect("valid");
        let verdict = rules
            .review_tool_call("shell", r#"{"command":"rm -rf /tmp"}"#)
            .expect("the rule fires");
        assert_eq!(verdict.act, Act::Block);
        assert_eq!(verdict.reason, "recursive delete");
    }

    /// A rule naming a tool must not fire for a different one — otherwise
    /// scoping a rule to `shell` would quietly gate every tool in the fleet.
    #[test]
    fn a_rule_scoped_to_one_tool_ignores_the_others() {
        let rules = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"secret","tool":"shell"}]}"#,
        ))
        .expect("valid");
        assert!(rules.review_tool_call("shell", "secret").is_some());
        assert!(rules.review_tool_call("fs", "secret").is_none());
    }

    #[test]
    fn a_rule_may_ask_instead_of_blocking() {
        let rules = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"deploy","decision":"ask"}]}"#,
        ))
        .expect("valid");
        let verdict = rules
            .review_tool_call("shell", "deploy production")
            .expect("the rule fires");
        assert_eq!(verdict.act, Act::Ask);
    }

    /// The default reason names the pattern, so a rule written without one
    /// still tells the user what fired rather than refusing anonymously.
    #[test]
    fn a_rule_without_a_reason_still_explains_itself() {
        let rules = Rules::from_config(&config(r#"{"deny-tool-arguments":[{"pattern":"token"}]}"#))
            .expect("valid");
        let verdict = rules
            .review_tool_call("fs", "token")
            .expect("the rule fires");
        assert!(verdict.reason.contains("token"), "{}", verdict.reason);
    }

    #[test]
    fn an_unparseable_pattern_invalidates_the_set() {
        let error = Rules::from_config(&config(r#"{"deny-tool-arguments":[{"pattern":"["}]}"#))
            .expect_err("a broken pattern is an error");
        assert_eq!(error, RuleError::Pattern("[".to_owned()));
    }

    #[test]
    fn a_rule_without_a_pattern_is_an_error() {
        let error = Rules::from_config(&config(r#"{"deny-tool-arguments":[{"tool":"shell"}]}"#))
            .expect_err("a rule with nothing to match is an error");
        assert_eq!(error, RuleError::MissingPattern);
    }

    /// A misspelled decision must not silently become the weaker one.
    #[test]
    fn an_unrecognised_decision_is_an_error_rather_than_a_default() {
        let error = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"x","decision":"warn"}]}"#,
        ))
        .expect_err("`warn` is not a decision");
        assert_eq!(error, RuleError::Decision("warn".to_owned()));
    }

    #[test]
    fn text_with_nothing_to_redact_is_left_alone() {
        let rules = Rules::from_config(&config(r#"{"redact":[{"pattern":"sk-[a-z0-9]+"}]}"#))
            .expect("valid");
        assert!(rules.review_text("the answer is 42").is_none());
    }

    #[test]
    fn every_occurrence_is_replaced_not_just_the_first() {
        let rules = Rules::from_config(&config(
            r#"{"redact":[{"pattern":"sk-[a-z0-9]+","with":"[key]"}]}"#,
        ))
        .expect("valid");
        let TextVerdict::Redacted(text) =
            rules.review_text("sk-aaa then sk-bbb").expect("both match")
        else {
            panic!("a replace rule redacts");
        };
        assert_eq!(text, "[key] then [key]");
    }

    #[test]
    fn a_rule_without_a_replacement_uses_the_default() {
        let rules =
            Rules::from_config(&config(r#"{"redact":[{"pattern":"hunter2"}]}"#)).expect("valid");
        let TextVerdict::Redacted(text) = rules.review_text("pw: hunter2").expect("it matches")
        else {
            panic!("a replace rule redacts");
        };
        assert_eq!(text, "pw: [redacted]");
    }

    #[test]
    fn rules_compose_over_one_piece_of_text() {
        let rules = Rules::from_config(&config(
            r#"{"redact":[{"pattern":"alice","with":"A"},{"pattern":"bob","with":"B"}]}"#,
        ))
        .expect("valid");
        let TextVerdict::Redacted(text) = rules
            .review_text("alice met bob")
            .expect("both rules match")
        else {
            panic!("a replace rule redacts");
        };
        assert_eq!(text, "A met B");
    }

    /// Once an operator has said this text may not pass, handing back a
    /// partially rewritten version of it is not what they asked for.
    #[test]
    fn a_blocking_rule_wins_over_redaction_whatever_the_order() {
        let both = r#"{"redact":[{"pattern":"name","with":"X"},
                       {"pattern":"secret","decision":"block","reason":"a secret"}]}"#;
        let rules = Rules::from_config(&config(both)).expect("valid");
        let verdict = rules.review_text("name and secret").expect("it matches");
        assert_eq!(verdict, TextVerdict::Blocked("a secret".to_owned()));
    }

    #[test]
    fn an_unrecognised_text_decision_is_an_error() {
        let error = Rules::from_config(&config(r#"{"redact":[{"pattern":"x","decision":"ask"}]}"#))
            .expect_err("a text rule cannot ask; there is nobody to ask mid-stream");
        assert_eq!(error, RuleError::TextDecision("ask".to_owned()));
    }

    /// The two lists are independent: configuring one must not arm the other.
    #[test]
    fn tool_rules_and_text_rules_do_not_leak_into_each_other() {
        let rules =
            Rules::from_config(&config(r#"{"redact":[{"pattern":"secret"}]}"#)).expect("valid");
        assert!(rules.review_tool_call("shell", "secret").is_none());

        let rules =
            Rules::from_config(&config(r#"{"deny-tool-arguments":[{"pattern":"secret"}]}"#))
                .expect("valid");
        assert!(rules.review_text("secret").is_none());
    }

    /// Lookaround is not supported by the engine, and a pattern that uses it is
    /// a configuration error rather than a rule that silently never fires.
    #[test]
    fn lookaround_is_refused_by_the_engine_not_ignored() {
        let error = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"(?=secret)"}]}"#,
        ))
        .expect_err("the engine has no lookaround");
        assert_eq!(error, RuleError::Pattern("(?=secret)".to_owned()));
    }
}
