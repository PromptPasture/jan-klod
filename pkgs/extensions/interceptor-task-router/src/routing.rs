//! Task classification and route resolution: pure Rust, unit-tested natively.
//! [`parse_task`] reads back the classifier's constrained output; [`model_from_route`] takes the model from a route.

/// Built-in task types — the default `routing:` keys.
pub const BUILT_IN_TASKS: &[&str] = &[
    "code-generation",
    "code-review",
    "file-edit",
    "reasoning",
    "planning",
    "web-search",
    "research",
    "chat",
    "clarification",
    "agent-delegation",
];

/// Default task when classification fails: plain chat is the safe, cheapest route.
pub const DEFAULT_TASK: &str = "chat";

/// `GBNF` grammar constraining the classifier to exactly one of `tasks`.
#[must_use]
pub fn classifier_grammar(tasks: &[&str]) -> String {
    let alternatives = tasks
        .iter()
        .map(|task| format!("\"{task}\""))
        .collect::<Vec<_>>()
        .join(" | ");
    format!("root ::= {alternatives}")
}

/// Read the classifier's output into one of `tasks` (exact match first, else substring).
#[must_use]
pub fn parse_task(text: &str, tasks: &[&str]) -> Option<String> {
    let lowered = text.trim().to_lowercase();
    tasks
        .iter()
        .find(|task| lowered == ***task)
        .or_else(|| tasks.iter().find(|task| lowered.contains(**task)))
        .map(|task| (*task).to_string())
}

/// Pull the model out of a `<provider-instance>/<model>` routing entry.
#[must_use]
pub fn model_from_route(route: &str) -> Option<&str> {
    let model = route
        .split_once('/')
        .map_or(route, |(_provider, model)| model);
    Some(model.trim()).filter(|model| !model.is_empty())
}

/// The provider a route names but the runtime cannot honour. The fallback chain is fixed at boot,
/// so the model goes to whichever provider answers, not necessarily the named one.
#[must_use]
pub fn unhonoured_provider(route: &str) -> Option<&str> {
    route
        .split_once('/')
        .map(|(provider, _)| provider.trim())
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_lists_every_task() {
        let grammar = classifier_grammar(&["chat", "planning"]);
        assert_eq!(grammar, "root ::= \"chat\" | \"planning\"");
    }

    #[test]
    fn parse_task_matches_exact_then_substring() {
        assert_eq!(parse_task("chat", BUILT_IN_TASKS).as_deref(), Some("chat"));
        assert_eq!(
            parse_task("Task: code-review\n", BUILT_IN_TASKS).as_deref(),
            Some("code-review")
        );
        assert_eq!(parse_task("nonsense", BUILT_IN_TASKS), None);
    }

    #[test]
    fn a_route_yields_its_model_in_either_form() {
        assert_eq!(model_from_route("gpt-4o"), Some("gpt-4o"));
        assert_eq!(model_from_route("qwen2.5:7b"), Some("qwen2.5:7b"));
        assert_eq!(model_from_route("openai/gpt-4o"), Some("gpt-4o"));
        assert_eq!(model_from_route("ollama/qwen2.5:7b"), Some("qwen2.5:7b"));
        assert_eq!(model_from_route("openai/"), None);
        assert_eq!(model_from_route("  "), None);
    }

    #[test]
    fn a_provider_prefix_is_reported_rather_than_silently_dropped() {
        assert_eq!(unhonoured_provider("groq/llama-3.3-70b"), Some("groq"));
        assert_eq!(
            unhonoured_provider("gpt-4o"),
            None,
            "a bare model claims nothing"
        );
    }
}
