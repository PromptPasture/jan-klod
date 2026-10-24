//! Tier-0 language detection — pure-Rust gate.
//!
//! Reliable non-English text bypasses English heuristics; short or unreliable input stays eligible.

use whatlang::{detect, Lang};

/// Check if heuristics should run for `text`.
///
/// Returns `false` only for reliable non-English (goes to LLM tier directly).
pub fn is_heuristic_eligible(text: &str) -> bool {
    match detect(text) {
        Some(info) if info.is_reliable() => info.lang() == Lang::Eng,
        // Unreliable detection (e.g. "hi"): leave trivial greetings to heuristics, not a model call.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliable_english_prose_is_eligible() {
        assert!(is_heuristic_eligible(
            "Hello there, could you please tell me what the weather is like today?"
        ));
    }

    #[test]
    fn reliable_non_english_prose_is_not_eligible() {
        assert!(!is_heuristic_eligible(
            "Bonjour, pourriez-vous me dire quelle est la meilleure façon de \
             préparer un gâteau au chocolat aujourd'hui ?"
        ));
    }

    #[test]
    fn short_ambiguous_input_stays_eligible() {
        // Short text: too ambiguous to bypass; must reach greeting heuristics.
        for s in ["hi", "ok", "hey", "yo"] {
            assert!(is_heuristic_eligible(s), "{s:?} should stay eligible");
        }
    }

    #[test]
    fn empty_input_is_eligible() {
        assert!(is_heuristic_eligible(""));
        assert!(is_heuristic_eligible("   "));
    }
}
