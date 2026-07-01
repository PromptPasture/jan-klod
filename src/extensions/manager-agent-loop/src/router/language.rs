//! Tier-0 language detection — a pure-Rust gate (no model call) deciding whether
//! the microsecond English heuristics are even applicable.
//!
//! The heuristics ([`super::heuristics`]) are English phrase matches, so they
//! must never fire on another language. `whatlang` is a statistical detector: on
//! very short strings (`"hi"`, `"ok"`) it is unreliable, so we only *bypass* the
//! heuristics when we are confident the text is a reliable **non-English**
//! language. Empty, too-short-to-classify, unreliable, or reliably-English text
//! all stay eligible for the heuristic tier.

use whatlang::{detect, Lang};

/// Whether the English heuristic tier should be consulted for `text`.
///
/// Returns `false` only for a *reliable non-English* detection — that input goes
/// straight to the LLM classifier tier. Everything else (no detection, low
/// confidence, or reliable English) returns `true`.
pub fn is_heuristic_eligible(text: &str) -> bool {
    match detect(text) {
        Some(info) if info.is_reliable() => info.lang() == Lang::Eng,
        // No detection or low confidence — typical for short inputs like "hi".
        // Let the English heuristics have a look rather than paying for a model
        // call on what is probably a trivial greeting.
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
        // A clearly French sentence — long enough for a reliable detection.
        assert!(!is_heuristic_eligible(
            "Bonjour, pourriez-vous me dire quelle est la meilleure façon de \
             préparer un gâteau au chocolat aujourd'hui ?"
        ));
    }

    #[test]
    fn short_ambiguous_input_stays_eligible() {
        // Too short for a confident detection — must not be bypassed, or the
        // greeting heuristics would never see it.
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
