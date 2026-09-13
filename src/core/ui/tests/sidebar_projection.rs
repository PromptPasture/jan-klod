#![allow(missing_docs)]
//! #104's Acceptance line 7: "a grep-style test asserts the sidebar issues no
//! `Command` — it is a projection only." Modelled on `theme.rs`'s own
//! `no_colour_literal_survives_outside_this_module`: a plain grep over the
//! module's source, comments stripped, for the two words that would mean it
//! had grown a way to drive anything (`Command`, which dispatches the `/`
//! menu, and `Transport`, which is the only thing that can send one).
//!
//! This lives in `tests/` rather than in `src/sidebar.rs` itself so the grep
//! never has to read its own assertion text back — a check for `"Command"`
//! sitting in the same file it scans would trip on itself.

use std::fs;
use std::path::Path;

#[test]
fn the_sidebar_issues_no_command_and_holds_no_transport() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sidebar.rs");
    let text = fs::read_to_string(&path).expect("src/sidebar.rs exists");

    let mut checked = 0usize;
    for (n, line) in text.lines().enumerate() {
        // Everything from the first `//` is a comment; a `//` inside a string
        // literal would only make this check miss something, never invent one.
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
