//! Tier-0 language detection — pure-Rust gate.
//!
//! English heuristics must not fire on non-English text. `whatlang` is unreliable
//! on short strings, so we only bypass heuristics for *reliably non-English*.
//! Empty, short, unreliable, or English text stays eligible for heuristics.

use whatlang::{detect, Lang};

/// Check if heuristics should run for `text`.
///
/// Returns `false` only for reliable non-English (goes to LLM tier directly).
/// Empty, short, unreliable, or English text returns `true`.
pub fn is_heuristic_eligible(text: &str) -> bool {
    match detect(text) {
        Some(info) if info.is_reliable() => info.lang() == Lang::Eng,
        // Unreliable or no detection (typical for short inputs like "hi").
        // Let heuristics run rather than model-call trivial greetings.
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
        // Long French text detected reliably.
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
