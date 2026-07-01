//! Layered intent router — the first gate every prompt passes before the agent
//! loop, classifying it as [`Intent::Simple`] (answer inline, no loop) or
//! [`Intent::Agentic`] (enter the step controller).
//!
//! Three tiers, cheapest first (see `docs/concepts/small-model-harness.md`):
//!
//! 1. **Language detection** ([`language`]) — pure Rust, microseconds. A reliable
//!    non-English detection bypasses the English heuristics.
//! 2. **Heuristics** ([`heuristics`]) — English exact-phrase matches, microseconds,
//!    zero model cost. Settles obvious greetings/acks as [`Intent::Simple`].
//! 3. **LLM classifier** — a single constrained-decoding call to the active
//!    `llm-provider`, invoked only when the cheap tiers do not settle it. Handles
//!    all languages. Injected as a closure so this module stays pure and unit-
//!    testable; the wasm component supplies the real provider-backed call.

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

/// `GBNF` grammar for the tier-3 classifier: the model must emit exactly one of
/// the two labels. Passed as the `grammar` field of the completion request so a
/// constrained-decoding backend (`llama.cpp` / `vLLM` guided decoding) cannot
/// produce anything else. Backends that ignore `grammar` still round-trip through
/// [`parse_intent`].
pub const CLASSIFIER_GRAMMAR: &str = "root ::= \"simple\" | \"agentic\"";

/// Classify `prompt` through the layered tiers.
///
/// `llm_classifier` is consulted **only** when the cheap tiers do not settle the
/// prompt (heuristics pass, or the input is reliably non-English). It receives
/// the raw prompt and returns the tier-3 verdict.
pub fn classify(prompt: &str, llm_classifier: impl FnOnce(&str) -> Intent) -> Intent {
    if language::is_heuristic_eligible(prompt) && heuristics::is_simple(prompt) {
        return Intent::Simple;
    }
    llm_classifier(prompt)
}

/// Parse the tier-3 classifier's raw completion text into an [`Intent`].
///
/// Ambiguous output (neither label present, e.g. a backend that ignored the
/// grammar and rambled) defaults to [`Intent::Agentic`] — the safe choice, since
/// mis-skipping the loop on a real task is worse than an unnecessary loop.
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
        assert!(!called.get(), "LLM tier must not run when heuristics settle it");
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
        // A prompt the heuristics do not catch but the model deems simple.
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
        assert_eq!(parse_intent("I'm not sure how to answer that"), Intent::Agentic);
    }

    #[test]
    fn non_english_bypasses_heuristics_to_the_model() {
        let called = Cell::new(false);
        // Reliable French — skips the English heuristics straight to the model.
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
