//! Session window state on `App`: `switch_session` REPLACES the transcript
//! (it is the only unload path today), clears the outbound queue, and
//! `drop_tail_from` bounds its truncation. These pin the contracts the
//! turn-based dynamic window must preserve.

use kymido_tui::app::App;
use web_state::types::{ChatMessage, MessagePart};

fn msg(text: &str) -> ChatMessage {
    ChatMessage {
        id: format!("m-{text}"),
        role: "user".to_string(),
        content: text.to_string(),
        reasoning: String::new(),
        tool_calls: vec![],
        parts: vec![MessagePart::Text(text.to_string())],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

fn msgs(prefix: &str, n: usize) -> Vec<ChatMessage> {
    (1..=n).map(|i| msg(&format!("{prefix}-{i:02}"))).collect()
}

#[test]
fn switch_session_loads_the_given_history_in_order() {
    let mut app = App::new();
    app.switch_session("s1", msgs("a", 3));

    let texts: Vec<String> = app.messages().iter().map(|m| m.content.clone()).collect();
    assert_eq!(texts, vec!["a-01", "a-02", "a-03"]);
}

#[test]
fn second_switch_replaces_not_appends() {
    let mut app = App::new();
    app.switch_session("s1", msgs("a", 3));
    app.switch_session("s2", msgs("b", 2));

    let texts: Vec<String> = app.messages().iter().map(|m| m.content.clone()).collect();
    assert_eq!(
        texts,
        vec!["b-01", "b-02"],
        "the old session's messages must be gone, not appended after"
    );
}

#[test]
fn switch_session_clears_the_outbound_queue() {
    let mut app = App::new();
    // T10 queue: type into the composer and press Enter — the prompt sits
    // in the outbound queue until the event loop ships it.
    for c in "queued prompt".chars() {
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(app.queued_count() > 0, "queue non-empty before the switch");

    app.switch_session("s2", msgs("b", 1));
    assert_eq!(
        app.queued_count(),
        0,
        "queued prompts must not follow into the new session (route §3 T4/T10)"
    );
}

#[test]
fn drop_tail_from_truncates_past_the_keep_point() {
    let mut app = App::new();
    app.switch_session("s1", msgs("a", 5));

    app.drop_tail_from(2);
    let texts: Vec<String> = app.messages().iter().map(|m| m.content.clone()).collect();
    assert_eq!(texts, vec!["a-01", "a-02"], "everything past keep is gone");
}

#[test]
fn drop_tail_from_is_a_noop_when_keep_covers_the_transcript() {
    let mut app = App::new();
    app.switch_session("s1", msgs("a", 3));

    app.drop_tail_from(10);
    assert_eq!(
        app.messages().len(),
        3,
        "keep beyond the tail changes nothing"
    );
}

#[test]
fn transcript_grows_without_auto_eviction_today() {
    let mut app = App::new();
    app.switch_session("s1", msgs("a", 250));

    assert_eq!(
        app.messages().len(),
        250,
        "documents the current contract: no eviction within a session lifetime — \
         the turn-based window rework replaces this and must keep load bounded instead"
    );
}
