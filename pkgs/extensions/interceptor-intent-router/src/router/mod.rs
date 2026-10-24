//! Layered intent router: classifies a prompt as [`Intent::Simple`] or [`Intent::Agentic`].
//! Tiers run cheapest first: language gate, English heuristics, LLM classifier (see `docs/concepts/small-model-harness.md`).
//! The LLM tier is injected as a closure so this module stays pure and unit-testable.

mod heuristics;
mod language;

/// The router's verdict for a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Answerable directly with one completion — skip the agent loop.
    Simple,
    /// Needs the `ReAct` step controller (tools, planning, multi-step).
    Agentic,
}

/// GBNF grammar constraining the tier-3 classifier to exactly one label.
/// Backends that ignore `grammar` still go through [`parse_intent`].
pub const CLASSIFIER_GRAMMAR: &str = "root ::= \"simple\" | \"agentic\"";

/// Classify `prompt` through the layered tiers.
///
/// `llm_classifier` runs only when the cheap tiers do not settle the prompt.
pub fn classify(prompt: &str, llm_classifier: impl FnOnce(&str) -> Intent) -> Intent {
    if language::is_heuristic_eligible(prompt) && heuristics::is_simple(prompt) {
        return Intent::Simple;
    }
    llm_classifier(prompt)
}

/// Parse the tier-3 classifier's raw completion text into an [`Intent`].
///
/// Ambiguous output defaults to [`Intent::Agentic`]: wrongly skipping the loop is worse than an extra one.
#[must_use]
pub fn parse_intent(text: &str) -> Intent {
    let lowered = text.trim().to_lowercase();
    if lowered.contains("agentic") {
        Intent::Agentic
    } else if lowered.contains("simple") {
        Intent::Simple
    } else {
        Intent::Agentic
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn heuristic_simple_short_circuits_the_model() {
        let called = Cell::new(false);
        let intent = classify("hello", |_| {
            called.set(true);
            Intent::Agentic
        });
        assert_eq!(intent, Intent::Simple);
        assert!(
            !called.get(),
            "LLM tier must not run when heuristics settle it"
        );
    }

    #[test]
    fn english_multi_step_reaches_the_model() {
        let called = Cell::new(false);
        let intent = classify("Refactor the module and run the test suite", |_| {
            called.set(true);
            Intent::Agentic
        });
        assert!(called.get(), "LLM tier must run when heuristics pass");
        assert_eq!(intent, Intent::Agentic);
    }

    #[test]
    fn model_verdict_is_respected() {
        let intent = classify("What is the capital of France?", |_| Intent::Simple);
        assert_eq!(intent, Intent::Simple);
    }

    #[test]
    fn classifier_grammar_constrains_to_both_labels() {
        assert!(CLASSIFIER_GRAMMAR.contains("simple"));
        assert!(CLASSIFIER_GRAMMAR.contains("agentic"));
    }

    #[test]
    fn parse_intent_reads_the_labels() {
        assert_eq!(parse_intent("simple"), Intent::Simple);
        assert_eq!(parse_intent(" AGENTIC \n"), Intent::Agentic);
        assert_eq!(parse_intent("Intent: simple"), Intent::Simple);
    }

    #[test]
    fn parse_intent_defaults_ambiguous_to_agentic() {
        assert_eq!(parse_intent(""), Intent::Agentic);
        assert_eq!(
            parse_intent("I'm not sure how to answer that"),
            Intent::Agentic
        );
    }

    #[test]
    fn non_english_bypasses_heuristics_to_the_model() {
        let called = Cell::new(false);
        let intent = classify(
            "Peux-tu réécrire cette fonction et ajouter des tests unitaires complets ?",
            |_| {
                called.set(true);
                Intent::Agentic
            },
        );
        assert!(called.get(), "non-English must reach the LLM tier");
        assert_eq!(intent, Intent::Agentic);
    }
}
