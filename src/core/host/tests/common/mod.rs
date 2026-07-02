// Each test binary compiles this module independently; not every binary needs every item.
#![allow(dead_code)]

use std::path::PathBuf;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;

/// Removes the test temp directory on drop — even if the test panics.
pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Resolves the repo root from the crate manifest directory.
pub fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

/// A canned chat-completions reply, so the routed provider completes offline.
pub fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}
