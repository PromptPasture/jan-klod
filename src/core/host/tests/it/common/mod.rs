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
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."]
        .iter()
        .collect()
}

/// Set this to turn "guest not staged, skip the test" into a hard failure.
/// `make gate` exports it; see [`guests_staged`].
const REQUIRE: &str = "JK_REQUIRE_GUESTS";

/// Whether every named guest is staged in `ext/` — and therefore whether the
/// caller may run.
///
/// A skipped test reports as passing, so an unstaged `ext/` can hide a broken
/// assertion indefinitely (it has happened). Skipping is allowed only when
/// nobody has asked for the real thing: with `JK_REQUIRE_GUESTS` set (`make
/// gate`, CI), a missing guest panics with what to run instead of skipping.
pub fn guests_staged(guests: &[&str]) -> bool {
    let ext_dir = repo_root().join("ext");
    let absent: Vec<&str> = guests
        .iter()
        .copied()
        .filter(|g| !ext_dir.join(g).exists())
        .collect();
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

/// Whether `program` answers a version query. Tries both spellings: `TinyGo`
/// only answers the bare `version` subcommand, not `--version`, so checking
/// only one can misreport an installed compiler as absent.
fn runnable(program: &str) -> bool {
    ["--version", "version"].iter().any(|flag| {
        std::process::Command::new(program)
            .arg(flag)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// Whether an optional developer toolchain is present, with **no** requirement
/// that it be. Distinct from [`tool_available`]: this is for a genuinely
/// optional toolchain (e.g. `tinygo`/`wkg`, which CI doesn't carry) where a hard
/// failure would just teach people to unset `JK_REQUIRE_GUESTS`. Skip is
/// announced, not silent.
pub fn optional_tool(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    eprintln!("skipping: optional toolchain `{program}` is not installed");
    false
}

/// Whether an external program a test needs is on `PATH`. Same policy as
/// [`guests_staged`] and for the same reason: a test that quietly vanishes when
/// a prerequisite is missing proves nothing while looking green.
pub fn tool_available(program: &str) -> bool {
    if runnable(program) {
        return true;
    }
    assert!(
        std::env::var(REQUIRE).is_err(),
        "{REQUIRE} is set, so this test must not be skipped, but `{program}` is not \
         on PATH"
    );
    eprintln!("skipping: `{program}` is not available");
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
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}
