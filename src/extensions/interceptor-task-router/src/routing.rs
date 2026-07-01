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
    route
        .split_once('/')
        .map(|(_provider, model)| model)
        .filter(|model| !model.is_empty())
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
    fn model_from_route_splits_provider_and_model() {
        assert_eq!(model_from_route("openai/gpt-4o"), Some("gpt-4o"));
        assert_eq!(model_from_route("ollama/qwen2.5:7b"), Some("qwen2.5:7b"));
        assert_eq!(model_from_route("no-slash"), None);
        assert_eq!(model_from_route("openai/"), None);
    }
}
