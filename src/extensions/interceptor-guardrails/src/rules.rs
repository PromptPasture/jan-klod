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

/// The configured rule set. Empty is the normal state: a guardrail nobody
/// configured must not change what the loop does.
#[derive(Debug, Default)]
pub struct Rules {
    deny_tool_arguments: Vec<DenyRule>,
}

impl Rules {
    /// Build the rule set from this extension's `config.yaml` section, as the
    /// JSON object `host-config::all` returns.
    ///
    /// Recognised key, optional:
    ///
    /// ```yaml
    /// deny-tool-arguments:
    ///   - pattern: "rm +-rf"        # required
    ///     tool: shell               # optional; every tool when absent
    ///     reason: "recursive delete"# optional
    ///     decision: block           # optional; `block` or `ask`, default block
    /// ```
    ///
    /// A section that is absent, null, or carries no rules yields an empty set.
    ///
    /// # Errors
    /// Returns the first malformed rule. One bad rule invalidates the set —
    /// see the module doc for why.
    pub fn from_config(section: &serde_json::Value) -> Result<Self, RuleError> {
        let Some(raw) = section
            .get("deny-tool-arguments")
            .and_then(|v| v.as_array())
        else {
            return Ok(Self::default());
        };
        let mut deny_tool_arguments = Vec::with_capacity(raw.len());
        for rule in raw {
            let pattern = rule
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .ok_or(RuleError::MissingPattern)?;
            let compiled =
                Regex::new(pattern).map_err(|_| RuleError::Pattern(pattern.to_owned()))?;
            deny_tool_arguments.push(DenyRule {
                pattern: compiled,
                tool: rule
                    .get("tool")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                reason: rule
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(
                        || format!("matches the guardrail `{pattern}`"),
                        ToOwned::to_owned,
                    ),
                act: Act::parse(rule.get("decision").and_then(serde_json::Value::as_str))?,
            });
        }
        Ok(Self {
            deny_tool_arguments,
        })
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
}

#[cfg(test)]
mod tests {
    use super::{Act, RuleError, Rules};

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
