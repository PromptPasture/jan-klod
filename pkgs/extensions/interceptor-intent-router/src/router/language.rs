//! Tier-0 language gate: reliable non-English text bypasses English heuristics; short or unreliable input stays eligible.

use whatlang::{detect, Lang};

/// `false` only for reliable non-English text, which goes to the LLM tier.
pub fn is_heuristic_eligible(text: &str) -> bool {
    match detect(text) {
        Some(info) if info.is_reliable() => info.lang() == Lang::Eng,
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
