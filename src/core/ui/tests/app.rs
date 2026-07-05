#![allow(missing_docs)]
use jan_klod::app::{App, Entry, Who};

#[test]
fn typing_and_backspace_edit_the_input() {
    let mut app = App::default();
    for c in "hii".chars() {
        app.push_char(c);
    }
    app.backspace();
    assert_eq!(app.input, "hi");
}

#[test]
fn submit_records_the_user_line_and_clears_input() {
    let mut app = App::default();
    "  hello  ".chars().for_each(|c| app.push_char(c));
    let sent = app.take_submission();
    assert_eq!(sent.as_deref(), Some("hello"), "trimmed message returned");
    assert!(app.input.is_empty(), "input cleared after submit");
    assert_eq!(app.transcript, vec![Entry { who: Who::You, text: "hello".into() }]);
}

#[test]
fn blank_submit_sends_nothing() {
    let mut app = App::default();
    "   ".chars().for_each(|c| app.push_char(c));
    assert_eq!(app.take_submission(), None);
    assert!(app.transcript.is_empty());
}

#[test]
fn answers_and_errors_append_to_the_transcript() {
    let mut app = App::default();
    app.take_submission();
    "ask".chars().for_each(|c| app.push_char(c));
    app.take_submission();
    app.record_answer("the reply");
    app.record_error("boom");
    let kinds: Vec<Who> = app.transcript.iter().map(|e| e.who).collect();
    assert_eq!(kinds, vec![Who::You, Who::Klod, Who::Error]);
    assert_eq!(app.transcript[1].text, "the reply");
}
