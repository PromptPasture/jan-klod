//! Telegram update parsing and offset advance.

use jan_klod_core::telegram::{next_offset, parse_updates, Update};

fn updates_body(update_id: i64, chat_id: i64, text: &str) -> Vec<u8> {
    serde_json::json!({
        "ok": true,
        "result": [{
            "update_id": update_id,
            "message": { "message_id": 1, "chat": { "id": chat_id }, "text": text }
        }]
    })
    .to_string()
    .into_bytes()
}

#[test]
fn parse_updates_extracts_text_messages() {
    let updates = parse_updates(&updates_body(10, 42, "hello"));
    assert_eq!(
        updates,
        vec![Update {
            update_id: 10,
            chat_id: 42,
            text: "hello".into()
        }]
    );
}

#[test]
fn parse_updates_skips_non_text_and_bad_shapes() {
    let body = serde_json::json!({
        "result": [
            { "update_id": 1, "message": { "chat": { "id": 5 } } },
            { "update_id": 2, "edited_message": { "text": "x" } },
            { "update_id": 3, "message": { "chat": { "id": 7 }, "text": "ok" } }
        ]
    })
    .to_string();
    let updates = parse_updates(body.as_bytes());
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].chat_id, 7);
}

#[test]
fn next_offset_is_one_past_the_highest_id() {
    let updates = vec![
        Update {
            update_id: 4,
            chat_id: 1,
            text: "a".into(),
        },
        Update {
            update_id: 9,
            chat_id: 1,
            text: "b".into(),
        },
    ];
    assert_eq!(next_offset(&updates), Some(10));
    assert_eq!(next_offset(&[]), None);
}
