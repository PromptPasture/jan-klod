#![allow(missing_docs)]
use jan_klod::app::{App, Entry, Prompt, Who};

#[test]
fn typing_and_backspace_edit_the_input() {
    let mut app = App::default();
    for c in "hii".chars() {
        app.push_char(c);
    }
    app.backspace();
    assert_eq!(app.input(), "hi");
}

#[test]
fn submit_records_the_user_line_and_clears_input() {
    let mut app = App::default();
    "  hello  ".chars().for_each(|c| app.push_char(c));
    let sent = app.take_submission();
    assert_eq!(sent.as_deref(), Some("hello"), "trimmed message returned");
    assert!(app.input().is_empty(), "input cleared after submit");
    assert_eq!(
        app.transcript,
        vec![Entry::Message {
            who: Who::You,
            text: "hello".into()
        }]
    );
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
    let kinds: Vec<Who> = app
        .transcript
        .iter()
        .filter_map(|e| match e {
            Entry::Message { who, .. } => Some(*who),
            Entry::Tool(_) => None,
        })
        .collect();
    assert_eq!(kinds, vec![Who::You, Who::Klod, Who::Error]);
    match &app.transcript[1] {
        Entry::Message { text, .. } => assert_eq!(text, "the reply"),
        Entry::Tool(tool) => panic!("expected a message, got the tool block {tool:?}"),
    }
}

#[test]
fn selecting_an_option_and_answering_clears_the_wait() {
    let mut app = App::default();
    app.ask(Prompt {
        session: "s1".into(),
        question: "Allow tool `bash`?".into(),
        options: vec!["yes".into(), "no".into()],
        default: "no".into(),
    });
    assert!(app.pending_prompt().is_some(), "the turn is waiting");

    app.prompt_jump(1); // "yes"
    assert_eq!(
        app.take_answer(),
        Some(("s1".to_string(), "yes".to_string()))
    );
    assert!(app.pending_prompt().is_none(), "answering clears the wait");
}

#[test]
fn an_unanswered_prompt_takes_the_prompts_own_default() {
    let mut app = App::default();
    app.ask(Prompt {
        session: "s1".into(),
        question: "Allow tool `bash`?".into(),
        options: vec!["yes".into(), "no".into()],
        default: "no".into(),
    });
    // Enter with nothing else pressed must do the safe thing: the initial
    // selection is already `default`.
    assert_eq!(
        app.take_answer(),
        Some(("s1".to_string(), "no".to_string()))
    );
}

#[test]
fn taking_an_answer_with_nothing_pending_yields_nothing() {
    let mut app = App::default();
    "hello".chars().for_each(|c| app.push_char(c));
    assert_eq!(app.take_answer(), None);
    assert_eq!(app.input(), "hello", "an ordinary message is left alone");
}
