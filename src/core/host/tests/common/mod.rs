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

/// Set this to turn "guest not staged, skip the test" into a hard failure.
/// `make gate` exports it; see [`guests_staged`].
const REQUIRE: &str = "JK_REQUIRE_GUESTS";

/// Whether every named guest is staged in `ext/` — and therefore whether the
/// caller may run.
///
/// **Skipping is how a broken assertion hides.** These tests skip when `ext/` is
/// unstaged so `cargo test` works before `make ext`, and that is genuinely useful
/// — but a skipped test reports as passing, so an assertion that could never hold
/// looks green for as long as nobody stages the guests. That is not hypothetical:
/// `shipped_defaults` asserted `!report.contains("missing")` against a report
/// whose summary line always reads "0 missing", and it sat green for a full day
/// because it never actually ran.
///
/// So the skip is allowed only when nobody has asked for the real thing. With
/// `JK_REQUIRE_GUESTS` set — which `make gate` and CI do — a missing guest panics
/// with what to run, instead of quietly reporting success.
pub fn guests_staged(guests: &[&str]) -> bool {
    let ext_dir = repo_root().join("ext");
    let absent: Vec<&str> =
        guests.iter().copied().filter(|g| !ext_dir.join(g).exists()).collect();
    if absent.is_empty() {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but {absent:?} \
         are not staged in {} — run `make ext`",
        ext_dir.display()
    );
    eprintln!("skipping: {absent:?} not staged — run `make ext`");
    false
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
