//! Tier-1 heuristics — English, microseconds, zero model cost.
//!
//! A short curated set of exact phrases (plus a few greeting prefixes) that are
//! unambiguously *simple*: they can be answered inline with no tools, planning,
//! or multi-step loop. Anything not matched here "passes" to the LLM classifier
//! tier. Rules are **grouped by category**, not a flat pile — adding one is a
//! single line in the right `const`. Keep each group conservative: a false
//! `simple` skips the agent loop entirely, so when in doubt, let it pass.

/// Greetings answerable with a greeting.
const GREETINGS: &[&str] = &[
    "hi", "hello", "hey", "yo", "hiya", "howdy", "sup", "heya", "hullo",
    "greetings", "morning", "evening", "whats up", "what's up", "wassup",
];

/// Greeting *prefixes* — the phrase may carry a trailing name/clause.
const GREETING_PREFIXES: &[&str] = &["good morning", "good afternoon", "good evening", "good day"];

/// Farewells.
const FAREWELLS: &[&str] = &[
    "bye", "goodbye", "good bye", "see you", "see ya", "cya", "later",
    "catch you later", "take care", "farewell", "good night", "goodnight", "gn",
];

/// Affirmations, acknowledgements, and negations — conversational glue.
const AFFIRMATIONS: &[&str] = &[
    "yes", "yeah", "yep", "yup", "no", "nope", "nah", "ok", "okay", "k", "kk",
    "sure", "sounds good", "got it", "understood", "makes sense", "cool", "nice",
    "great", "awesome", "perfect", "fine", "alright", "right", "indeed", "agreed",
];

/// Thanks / politeness.
const COURTESIES: &[&str] = &[
    "thanks", "thank you", "thank you very much", "thanks a lot", "thx", "ty",
    "cheers", "much appreciated", "appreciate it", "no thanks", "no thank you",
    "please", "pardon", "sorry", "excuse me", "my bad",
];

/// Clarifications / conversation control.
const CLARIFICATIONS: &[&str] =
    &["never mind", "nevermind", "forget it", "ignore that", "as i said", "anyway", "moving on"];

/// Meta-queries about the assistant itself — answerable from a canned identity,
/// no tools or planning needed.
const META_QUERIES: &[&str] = &[
    "who are you", "what are you", "what can you do", "what do you do", "help",
    "what is your name", "what's your name", "how do you work", "are you there",
    "can you hear me", "what are your capabilities",
];

/// Short filler interjections.
const FILLERS: &[&str] = &["hmm", "huh", "oh", "ah", "wow", "lol", "haha", "hey there", "hi there"];

/// Every exact-match group, checked after normalisation.
const EXACT_GROUPS: &[&[&str]] = &[
    GREETINGS,
    FAREWELLS,
    AFFIRMATIONS,
    COURTESIES,
    CLARIFICATIONS,
    META_QUERIES,
    FILLERS,
];

/// Whether `text` is an obvious *simple* intent by heuristic alone.
///
/// Empty input counts as simple (there is nothing to run an agent loop over).
/// Otherwise the normalised text must exactly match a phrase in one of the
/// [`EXACT_GROUPS`] or start with a [`GREETING_PREFIXES`] entry.
pub fn is_simple(text: &str) -> bool {
    let normalized = normalize(text);
    if normalized.is_empty() {
        return true;
    }
    if EXACT_GROUPS
        .iter()
        .any(|group| group.contains(&normalized.as_str()))
    {
        return true;
    }
    GREETING_PREFIXES
        .iter()
        .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix} ")))
}

/// Lower-case, drop punctuation (apostrophes kept so `"what's up"` still
/// matches), and collapse whitespace — so `"Hello!"`, `"  hello  "`,
/// `"who are you?"`, and `"good morning, Klod"` all normalise to their rule form.
fn normalize(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' })
        .collect();
    // Collapse any run of whitespace to a single space and trim the ends.
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_are_simple() {
        for s in ["hi", "Hello", "HEY!", "  hi  ", "Good morning", "good morning, Klod"] {
            assert!(is_simple(s), "{s:?} should be simple");
        }
    }

    #[test]
    fn affirmations_and_courtesies_are_simple() {
        for s in ["yes", "Okay.", "thanks!", "Thank you very much", "no thanks", "got it"] {
            assert!(is_simple(s), "{s:?} should be simple");
        }
    }

    #[test]
    fn meta_queries_are_simple() {
        for s in ["Who are you?", "what can you do", "Help", "What's your name?"] {
            assert!(is_simple(s), "{s:?} should be simple");
        }
    }

    #[test]
    fn empty_is_simple() {
        assert!(is_simple(""));
        assert!(is_simple("   "));
    }

    #[test]
    fn multi_step_tasks_pass() {
        for s in [
            "Refactor the auth module and add tests",
            "Search the web for the latest Rust release and summarise it",
            "What is the capital of France and how far is it from Berlin?",
            "delete file",
            "build the project",
            "deploy",
        ] {
            assert!(!is_simple(s), "{s:?} should pass to the LLM tier");
        }
    }

    #[test]
    fn greeting_prefix_does_not_over_match() {
        // "good" alone is not a greeting; "good morning" is.
        assert!(!is_simple("good"));
        assert!(is_simple("good morning"));
    }
}
