//! `tool-web-search` — search the web through a configured provider API, returning results as readable text.
//!
//! `{ "query": "rust language" }` → plain text results one per line.
//!
//! ## Configuration and provider
//!
//! The tool reads `base-url` (the search API endpoint) and `api-key` (${VAR} environment variable
//! reference) from `host-config`. The operator's choice of base-url determines which search
//! provider is used and implicitly grants egress to that origin.
//!
//! ## Why disabled by default
//!
//! Web search results are reads, but queries are data that disclose what the model is working on,
//! and the API key is a credential. `tool.web-search` is `enabled: false`; enabling is a
//! deployment decision.

/// Request timeout in milliseconds; a search that will not answer is not an answer.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
const TIMEOUT_MS: u32 = 10_000;

// Pure logic: unit-tested natively; the CM glue only compiles for wasm32.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod search {
    use std::fmt::Write as _;

    /// URL encode a string for use in query parameters.
    /// Encodes spaces as %20 and other reserved characters.
    fn url_encode(s: &str) -> String {
        let mut result = String::new();
        for ch in s.chars() {
            match ch {
                'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                    result.push(ch);
                }
                ' ' => result.push_str("%20"),
                _ => {
                    for byte in ch.to_string().as_bytes() {
                        let _ = write!(result, "%{byte:02X}");
                    }
                }
            }
        }
        result
    }

    /// Construct a GET URL for the search provider.
    ///
    /// # Errors
    /// Returns an error if required config is missing (`base_url` or `api_key`).
    pub fn construct_url(base_url: &str, query: &str, api_key: &str) -> Result<String, String> {
        if base_url.is_empty() {
            return Err("base-url is not configured".to_string());
        }
        if api_key.is_empty() {
            return Err("api-key is not configured or environment variable not found".to_string());
        }
        if query.trim().is_empty() {
            return Err("query cannot be empty".to_string());
        }

        // URL encode the query string
        let encoded_query = url_encode(query);

        // Construct the URL with query parameters
        // Assume base_url doesn't have query params; separate with ? if no existing ?
        let separator = if base_url.contains('?') { "&" } else { "?" };
        let url = format!("{base_url}{separator}q={encoded_query}&api_key={api_key}");
        Ok(url)
    }

    /// Parse JSON search results and format as plain text.
    ///
    /// Expected JSON structure (typical of `SerpAPI` and similar providers):
    /// ```json
    /// {
    ///   "organic_results": [
    ///     {"title": "...", "snippet": "...", "link": "..."},
    ///     ...
    ///   ]
    /// }
    /// ```
    #[must_use]
    pub fn format_results(json_body: &str) -> String {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json_body) else {
            return "(failed to parse search results as JSON)".to_string();
        };

        // Try to extract results from common provider formats
        value
            .get("organic_results")
            .and_then(serde_json::Value::as_array)
            .map_or_else(
                || "(unexpected search result format; expected organic_results array)".to_string(),
                |results| {
                    let formatted: Vec<String> = results
                        .iter()
                        .map(|result| {
                            let title = result
                                .get("title")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("(no title)");
                            let snippet = result
                                .get("snippet")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("(no snippet)");
                            let link = result
                                .get("link")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("(no link)");
                            format!("{title} | {snippet} | {link}")
                        })
                        .collect();

                    if formatted.is_empty() {
                        "(no search results found)".to_string()
                    } else {
                        formatted.join("\n")
                    }
                },
            )
    }

    #[cfg(test)]
    mod tests {
        use super::{construct_url, format_results};

        #[test]
        fn construct_url_builds_correct_query() {
            let url = construct_url(
                "https://api.serpapi.com/search",
                "rust language",
                "test-key",
            )
            .unwrap();
            assert_eq!(
                url,
                "https://api.serpapi.com/search?q=rust%20language&api_key=test-key"
            );
        }

        #[test]
        fn construct_url_rejects_empty_query() {
            assert!(construct_url("https://api.serpapi.com/search", "", "test-key").is_err());
            assert!(construct_url("https://api.serpapi.com/search", "   ", "test-key").is_err());
        }

        #[test]
        fn construct_url_rejects_empty_base_url() {
            assert!(construct_url("", "query", "test-key").is_err());
        }

        #[test]
        fn construct_url_rejects_empty_api_key() {
            assert!(construct_url("https://api.serpapi.com/search", "query", "").is_err());
        }

        #[test]
        fn format_results_parses_organic_results() {
            let json = r#"{
                "organic_results": [
                    {"title": "Rust", "snippet": "A systems language", "link": "https://rust-lang.org"},
                    {"title": "Rust Book", "snippet": "Learn Rust", "link": "https://doc.rust-lang.org"}
                ]
            }"#;
            let output = format_results(json);
            assert!(output.contains("Rust"));
            assert!(output.contains("A systems language"));
            assert!(output.contains("https://rust-lang.org"));
            assert!(output.contains("Rust Book"));
            assert!(output.contains("Learn Rust"));
            assert!(output.contains("https://doc.rust-lang.org"));
        }

        #[test]
        fn format_results_handles_empty_results() {
            let json = r#"{"organic_results": []}"#;
            let output = format_results(json);
            assert!(output.contains("no search results"));
        }

        #[test]
        fn format_results_handles_invalid_json() {
            let output = format_results("not json");
            assert!(output.contains("failed to parse"));
        }

        #[test]
        fn format_results_handles_missing_fields() {
            let json = r#"{
                "organic_results": [
                    {"title": "Result"}
                ]
            }"#;
            let output = format_results(json);
            assert!(output.contains("Result"));
            assert!(output.contains("no snippet"));
            assert!(output.contains("no link"));
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::{search, TIMEOUT_MS};
    use core::cell::Cell;

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod bindings {
        wit_bindgen::generate!({ world: "tool-world", path: "../../../wit" });
    }

    use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
        ExtensionContext, Guest as Lifecycle, HealthStatus,
    };
    use bindings::exports::jan_klod::interfaces::tool_callable::{
        Guest as ToolCallable, ToolError, ToolMeta,
    };
    use bindings::jan_klod::interfaces::host_config;
    use bindings::jan_klod::interfaces::host_http;

    thread_local! {
        /// Base URL of the search provider, read once at `init`.
        static BASE_URL: Cell<String> = const { Cell::new(String::new()) };
        /// API key for the search provider, read once at `init`.
        static API_KEY: Cell<String> = const { Cell::new(String::new()) };
    }

    struct Component;

    impl Lifecycle for Component {
        fn init(_ctx: ExtensionContext) -> Result<(), String> {
            let raw = host_config::all().unwrap_or_else(|_| "{}".to_owned());
            let config = serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!("failed to parse config: {e}"))?;

            let base_url = config
                .get("base-url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let api_key = config
                .get("api-key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();

            BASE_URL.with(|u| u.set(base_url));
            API_KEY.with(|k| k.set(api_key));
            Ok(())
        }
        fn start() -> Result<(), String> {
            Ok(())
        }
        fn stop() {}
        fn health() -> HealthStatus {
            HealthStatus::Up
        }
    }

    impl ToolCallable for Component {
        fn meta() -> ToolMeta {
            ToolMeta {
                name: "web-search".to_string(),
                description: "Search the web through a configured search provider and return results as readable text. The query is sent to the provider configured at deployment time."
                    .to_string(),
                arguments_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "search query" }
                    },
                    "required": ["query"]
                })
                .to_string(),
            }
        }

        fn invoke(arguments: String) -> Result<String, ToolError> {
            let value: serde_json::Value =
                serde_json::from_str(&arguments).map_err(|_| ToolError::InvalidArguments)?;
            let query = value
                .get("query")
                .and_then(serde_json::Value::as_str)
                .ok_or(ToolError::InvalidArguments)?;

            let base_url = BASE_URL.with(std::cell::Cell::take);
            let api_key = API_KEY.with(std::cell::Cell::take);

            // Restore the values for next invocation
            BASE_URL.with(|u| u.set(base_url.clone()));
            API_KEY.with(|k| k.set(api_key.clone()));

            // Construct the request URL
            let url = match search::construct_url(&base_url, query, &api_key) {
                Ok(u) => u,
                Err(reason) => return Ok(format!("CONFIGURATION ERROR: {reason}")),
            };

            let request = host_http::HttpRequest {
                method: "GET".to_string(),
                url,
                headers: vec![host_http::HttpHeader {
                    name: "Accept".to_string(),
                    value: "application/json".to_string(),
                }],
                body: None,
                timeout_ms: TIMEOUT_MS,
            };

            match host_http::fetch(&request) {
                Ok(response) => {
                    let body = String::from_utf8_lossy(&response.body);
                    let formatted = search::format_results(&body);
                    Ok(guest_fs::truncate(formatted))
                }
                Err(err) => Ok(format!("FAILED: search request failed ({err:?})")),
            }
        }
    }

    #[allow(
        unsafe_code,
        missing_docs,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]
    mod glue {
        use super::{bindings, Component};
        bindings::export!(Component with_types_in bindings);
    }
}
