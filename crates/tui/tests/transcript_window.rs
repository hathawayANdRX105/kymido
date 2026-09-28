//! Transcript window slicing edges (T6): `window()` must skip messages
//! entirely above the viewport, slice a message straddling the top edge, and
//! tolerate an empty transcript without panicking.
//!
//! Driven through `ui::draw` on a TestBackend with the same helper shape as
//! `scroll_follow.rs`, so assertions run against real rendered cells rather
//! than the pure functions — the count/materialize consistency is already
//! pinned there; these pin the *slice* edges that function cannot see.
//!
//! Arithmetic note (all numbers come from scroll_follow.rs's measured
//! layout): at 80×24 the transcript viewport is 20 rows (footer + dock take
//! 4), so the following offset is `total − 20`.

use ratatui::{Terminal, backend::TestBackend, layout::Rect};

use kymido_tui::app::App;
use kymido_tui::ui;
use web_state::types::{ChatMessage, MessagePart};

// ——— helpers (same shape as tests/scroll_follow.rs) ———

fn user_msg(text: &str) -> ChatMessage {
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

fn history(msgs: Vec<ChatMessage>) -> App {
    let mut app = App::new();
    app.switch_session("s-window", msgs);
    app
}

/// Draw one frame at 80×24 and return the full screen text.
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw ok");
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        out.push('\n');
    }
    out
}

/// Geometry sync — the same entry the event loop calls before each draw.
fn sync(app: &mut App, width: u16, height: u16) {
    ui::sync_viewport(app, Rect::new(0, 0, width, height));
}

/// A tall 5-line message plus 17 one-liners: total = 22 rows, viewport 20,
/// following offset = 22 − 20 = 2 → the first visible row is line 3 of the
/// tall message, which therefore straddles the window top.
#[test]
fn message_straddling_the_top_renders_only_its_lower_slice() {
    let mut app = history(vec![
        user_msg("L1\nL2\nL3\nL4\nL5"),
        user_msg("m-01"),
        user_msg("m-02"),
        user_msg("m-03"),
        user_msg("m-04"),
        user_msg("m-05"),
        user_msg("m-06"),
        user_msg("m-07"),
        user_msg("m-08"),
        user_msg("m-09"),
        user_msg("m-10"),
        user_msg("m-11"),
        user_msg("m-12"),
        user_msg("m-13"),
        user_msg("m-14"),
        user_msg("m-15"),
        user_msg("m-16"),
        user_msg("m-17"),
    ]);
    sync(&mut app, 80, 24);

    let text = screen(&app);
    assert!(
        !text.contains("L1") && !text.contains("L2"),
        "head lines above the window must not render"
    );
    assert!(text.contains("L3"), "first visible line of the straddler");
    assert!(text.contains("L5"), "last line of the straddler");
    assert!(text.contains("m-01"), "first post-straddle message visible");
    assert!(
        text.contains("m-17"),
        "last message visible (tail anchored)"
    );
}

/// 40 one-liners: viewport 20, following offset = 40 − 20 = 20 →
/// m-01..m-20 are entirely above the window and must contribute nothing.
#[test]
fn messages_entirely_above_the_window_contribute_nothing() {
    let mut msgs: Vec<ChatMessage> = Vec::new();
    for i in 1..=40 {
        msgs.push(user_msg(&format!("m-{i:02}")));
    }
    let mut app = history(msgs);
    sync(&mut app, 80, 24);

    let text = screen(&app);
    assert!(
        !text.contains("m-20\n") && !text.contains("m-20 "),
        "last fully-above message invisible"
    );
    assert!(text.contains("m-21"), "first visible message");
    assert!(text.contains("m-40"), "tail message visible");
}

#[test]
fn empty_transcript_renders_without_panic() {
    let mut app = history(vec![]);
    sync(&mut app, 80, 24);

    // The meaningful edge is the absent panic: total_lines = 0, view_top
    // clamps, window() walks nothing.
    let text = screen(&app);
    assert!(
        !text.is_empty(),
        "frame still draws chrome around the transcript"
    );
}
