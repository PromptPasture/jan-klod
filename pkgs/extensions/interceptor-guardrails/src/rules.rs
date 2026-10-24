//! Guardrail rules as data: `regex` (linear time, no lookaround) and a malformed rule invalidates the whole set.

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Block,
    Ask,
}

impl Act {
    /// Absent means [`Act::Block`]; a typo is an error, never a downgrade.
    fn parse(raw: Option<&str>) -> Result<Self, RuleError> {
        match raw {
            None | Some("block") => Ok(Self::Block),
            Some("ask") => Ok(Self::Ask),
            Some(other) => Err(RuleError::Decision(other.to_owned())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleError {
    Pattern(String),
    MissingPattern,
    Decision(String),
    TextDecision(String),
}

impl RuleError {
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

#[derive(Debug)]
pub struct DenyRule {
    pattern: Regex,
    tool: Option<String>,
    reason: String,
    act: Act,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub reason: String,
    pub act: Act,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAct {
    Replace,
    Block,
}

impl TextAct {
    /// Absent means [`TextAct::Replace`]; blocking must be written down.
    fn parse(raw: Option<&str>) -> Result<Self, RuleError> {
        match raw {
            None | Some("replace") => Ok(Self::Replace),
            Some("block") => Ok(Self::Block),
            Some(other) => Err(RuleError::TextDecision(other.to_owned())),
        }
    }
}

const DEFAULT_REPLACEMENT: &str = "[redacted]";

#[derive(Debug)]
pub struct TextRule {
    pattern: Regex,
    with: String,
    reason: String,
    act: TextAct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextVerdict {
    Redacted(String),
    Blocked(String),
}

#[derive(Debug, Default)]
pub struct Rules {
    deny_tool_arguments: Vec<DenyRule>,
    redact: Vec<TextRule>,
}

impl Rules {
    /// One bad rule invalidates the whole set.
    pub fn from_config(section: &serde_json::Value) -> Result<Self, RuleError> {
        Ok(Self {
            deny_tool_arguments: Self::tool_rules(section)?,
            redact: Self::text_rules(section)?,
        })
    }

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

    /// The first rule that fires, in operator order (not most severe).
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

    /// A blocking rule is checked first and wins: a partial rewrite of blocked text is not what was asked.
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

fn compile(rule: &serde_json::Value) -> Result<(&str, Regex), RuleError> {
    let pattern = rule
        .get("pattern")
        .and_then(serde_json::Value::as_str)
        .ok_or(RuleError::MissingPattern)?;
    let compiled = Regex::new(pattern).map_err(|_| RuleError::Pattern(pattern.to_owned()))?;
    Ok((pattern, compiled))
}

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

    #[test]
    fn a_backreference_is_refused_like_lookaround() {
        let error = Rules::from_config(&config(r#"{"redact":[{"pattern":"(a)\\1"}]}"#))
            .expect_err("the engine has no backreferences");
        assert!(matches!(error, RuleError::Pattern(_)), "{error:?}");
    }

    #[test]
    fn an_inline_flag_works() {
        let rules =
            Rules::from_config(&config(r#"{"redact":[{"pattern":"(?i)secret"}]}"#)).expect("valid");
        assert!(rules.review_text("SECRET").is_some());
    }

    #[test]
    fn lookaround_is_refused_by_the_engine_not_ignored() {
        let error = Rules::from_config(&config(
            r#"{"deny-tool-arguments":[{"pattern":"(?=secret)"}]}"#,
        ))
        .expect_err("the engine has no lookaround");
        assert_eq!(error, RuleError::Pattern("(?=secret)".to_owned()));
    }
}
