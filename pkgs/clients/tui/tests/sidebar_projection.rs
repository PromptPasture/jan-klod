#![allow(missing_docs)]
//! #104: the sidebar is projection only, so `sidebar.rs` must not mention `Command` or `Transport`.
//!
//! Lives in `tests/` so the grep never reads its own assertion text back.

use std::fs;
use std::path::Path;

#[test]
fn the_sidebar_issues_no_command_and_holds_no_transport() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidebar.rs");
    let text = fs::read_to_string(&path).expect("src/sidebar.rs exists");

    let mut checked = 0usize;
    for (n, line) in text.lines().enumerate() {
        // A `//` inside a string literal can only make this miss, never falsely match.
        let code = line.split("//").next().unwrap_or_default();
        checked += 1;
        assert!(
            !code.contains("Command"),
            "sidebar.rs:{}: a `Command` reference — the sidebar is a \
             projection and must issue none:\n  {}",
            n + 1,
            line.trim()
        );
        assert!(
            !code.contains("Transport"),
            "sidebar.rs:{}: a `Transport` reference — the sidebar reads the \
             model, never the transport:\n  {}",
            n + 1,
            line.trim()
        );
    }
    assert!(
        checked > 20,
        "sidebar.rs looks empty — this test found nothing to check"
    );
}
