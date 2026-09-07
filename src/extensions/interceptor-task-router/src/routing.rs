//! Task classification + route resolution — pure Rust, unit-tested natively.
//!
//! The built-in task set mirrors the default `routing:` keys in `config.yaml`. A
//! constrained-decoding grammar forces the classifier to emit exactly one of
//! them; [`parse_task`] reads the label back, and [`model_from_route`] pulls the
//! model out of a `provider/model` routing entry. (User-defined task types are a
//! later refinement.)

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

/// The default task when classification fails or is ambiguous — plain chat is the
/// safe, cheapest route.
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

/// Read the classifier's output back into one of `tasks` (exact match first, then
/// substring for backends that ignore the grammar and ramble).
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

/// The provider name a route names but the runtime cannot honour.
///
/// A route may be written `provider/model`, and only the **model** is applied:
/// the fallback chain is fixed when the agent boots, so setting a model string
/// does not move the request to a different endpoint. `groq/llama-3.3-70b` sends
/// `llama-3.3-70b` to whichever provider answers — which is not groq, and will
/// fail as an unknown model or, worse, quietly resolve to something else.
///
/// Returning it lets the caller say so rather than discard it silently, which is
/// what this did before. Routing to a *provider* needs per-task reordering of the
/// chain and is not built; a bare model name is the form that means what it says.
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
        // Bare is the form that means what it says.
        assert_eq!(model_from_route("gpt-4o"), Some("gpt-4o"));
        assert_eq!(model_from_route("qwen2.5:7b"), Some("qwen2.5:7b"));
        // `provider/model` is accepted for the configs that already use it, but
        // only the model is applied.
        assert_eq!(model_from_route("openai/gpt-4o"), Some("gpt-4o"));
        assert_eq!(model_from_route("ollama/qwen2.5:7b"), Some("qwen2.5:7b"));
        assert_eq!(model_from_route("openai/"), None);
        assert_eq!(model_from_route("  "), None);
    }

    #[test]
    fn a_provider_prefix_is_reported_rather_than_silently_dropped() {
        // The chain is fixed at boot, so this half cannot be honoured. Saying so
        // is the difference between a documented limit and a wrong endpoint.
        assert_eq!(unhonoured_provider("groq/llama-3.3-70b"), Some("groq"));
        assert_eq!(
            unhonoured_provider("gpt-4o"),
            None,
            "a bare model claims nothing"
        );
    }
}
