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

/// Assistant message in the historical backfill shape (`parts` empty, body
/// in `content` — same shape as `web-state::convert::message_to_chat`); the
/// thinking block is driven by `reasoning`.
fn assistant_msg(reasoning: &str, text: &str) -> ChatMessage {
    ChatMessage {
        id: format!("a-{text}"),
        role: "assistant".to_string(),
        content: text.to_string(),
        reasoning: reasoning.to_string(),
        tool_calls: vec![],
        parts: vec![],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

/// Draw one frame at `width`×`height` and return the full screen text
/// (same shape as `scroll_follow.rs`'s `screen`).
fn screen_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
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

/// First 0-based row containing `needle` (same shape as
/// `scroll_follow.rs`'s `row_of`).
fn row_of(text: &str, needle: &str) -> Option<usize> {
    text.lines().position(|line| line.contains(needle))
}

/// 12 reasoning lines (> the thinking block's fold threshold 8): fold
/// render = head 3 + hidden-count row + tail 2. Each line is 46 columns —
/// one row at 80 cols, two rows at 44 (the two regimes catch a
/// count-vs-materialize wrap divergence).
fn long_reasoning() -> String {
    (0..12)
        .map(|i| format!("reason {i:02} 0123456789 0123456789 0123456789 end"))
        .collect::<Vec<_>>()
        .join("\n")
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

/// T21: empty reasoning takes zero rows (catches fabricated content —
/// historical session backfill (`web-state::convert`) always leaves
/// `reasoning` empty; a thinking block that renders a `◇ Thought` header
/// with nothing under it fabricates content, and the total row count is off
/// by one, desyncing the windowed transcript's scroll position from the
/// rendered picture).
#[test]
fn empty_reasoning_takes_zero_rows() {
    for width in [1usize, 20, 80] {
        assert_eq!(
            ui::thinking::rows("", width),
            0,
            "empty reasoning takes zero rows at width {width}"
        );
        assert!(
            ui::thinking::lines("", width).is_empty(),
            "empty reasoning materializes nothing at width {width}"
        );
        // Whitespace-only is the same class (trim-empty): a header with only
        // whitespace under it is fabricated too.
        assert_eq!(
            ui::thinking::rows("   \n \t", width),
            0,
            "whitespace-only reasoning takes zero rows at width {width}"
        );
        assert!(ui::thinking::lines("   \n \t", width).is_empty());
    }

    // Message level: an assistant message with empty reasoning (historical
    // shape) counts exactly its content rows; non-empty reasoning adds
    // exactly the block's rows (header 1 + body 3, no fold).
    let mut flat = history(vec![assistant_msg("", "body")]);
    sync(&mut flat, 80, 24);
    assert_eq!(
        flat.viewport().total(),
        1,
        "empty reasoning contributes zero rows"
    );
    assert!(
        !screen(&flat).contains("◇ Thought"),
        "no fabricated header may appear"
    );

    let mut with = history(vec![assistant_msg("r1\nr2\nr3", "body")]);
    sync(&mut with, 80, 24);
    assert_eq!(
        with.viewport().total(),
        5,
        "thinking block adds exactly 4 rows (header + 3 body)"
    );
}

/// T21: reasoning render and count agree (catches the rows/lines divergence
/// that desyncs scrolling: count side and materialize side drifting on the
/// thinking block's fold branch or wrap core makes the total line count
/// diverge from the actually rendered rows). Same shape as
/// `tests/scroll_follow.rs::window_render_matches_line_count`.
#[test]
fn reasoning_render_and_count_agree() {
    for width in [80u16, 44u16] {
        let mut app = history(vec![
            user_msg("FIRST-ROW marker"),
            assistant_msg(&long_reasoning(), "LAST-ROW text"),
        ]);
        sync(&mut app, width, 24);
        let total = app.viewport().total();
        if width == 80 {
            // user 1 + thinking block (header 1 + head 3 + hidden 1 + tail 2)
            // + body 1 = 9 — pins the fold arithmetic.
            assert_eq!(total, 9, "80-col total pins the fold math, got {total}");
        }
        // Screen height = total + footer 1 + dock 3 → transcript fits all
        // content; first row at screen top, last row at `total − 1`.
        let screen_h = total as u16 + 4;
        sync(&mut app, width, screen_h);
        assert_eq!(
            app.viewport().height(),
            total,
            "viewport height = full content height (col width {width})"
        );
        let text = screen_at(&app, width, screen_h);
        assert_eq!(
            row_of(&text, "FIRST-ROW"),
            Some(0),
            "width={width}: first row must be at screen top"
        );
        assert_eq!(
            row_of(&text, "LAST-ROW"),
            Some(total - 1),
            "width={width}: last row must be at row total−1 — count/materialize \
             divergence shows red here"
        );
    }
}

/// T21: reasoning renders before the text parts of the same message
/// (catches the block rendering after the text: the thinking block is the
/// model's pre-answer process and must precede that message's parts —
/// reversed order tells the story backwards).
#[test]
fn reasoning_precedes_text_parts() {
    let mut msg = assistant_msg("reason body R", "text part T");
    // Non-empty parts path (streaming projection shape).
    msg.parts = vec![MessagePart::Text("text part T".to_string())];
    let mut app = history(vec![msg]);
    sync(&mut app, 80, 24);

    let text = screen(&app);
    let header_row = row_of(&text, "◇ Thought").expect("thinking header visible");
    let body_row = row_of(&text, "reason body R").expect("thinking body visible");
    let text_row = row_of(&text, "text part T").expect("text part visible");
    assert!(
        header_row < text_row,
        "thinking header must precede the text part:\n{text}"
    );
    assert!(
        body_row < text_row,
        "thinking body must precede the text part:\n{text}"
    );
}

/// T23：宽字符按**显示格宽**断行——模型数出的行数必须等于画面上真占的行数。
///
/// 这条是 T23 的本体 bug。断行核心原来按 `chars().count()` 算宽度：一个 CJK
/// 字被当成 1 格，而它在终端里占 2 格。于是每个折行片段的实际宽度都是预算
/// 的两倍，**画面上的行数翻倍，而 `count_wrapped` 只按片段个数记账**——两者
/// 从此永久脱节，`ScrollModel::max_offset` / `view_top` 随之错位，滚动量算错。
///
/// 断言直接比**画面实际行数**与 `viewport().total()`：只调计数函数证明不了
/// 什么（它和渲染共用同一个错误的折行核心），只有画面能揭穿它。
#[test]
fn wide_characters_count_as_display_cells() {
    // 60 个 CJK 字 = 120 个显示格；按字符数断行只会算成 60 格。
    let body = "中".repeat(60);
    for width in [20u16, 40, 80] {
        let mut app = history(vec![user_msg(&body)]);
        sync(&mut app, width, 30);
        let drawn = screen_at(&app, width, 30);
        let rows_on_screen = drawn.lines().filter(|line| line.contains('中')).count();
        let counted = app.viewport().total();
        assert_eq!(
            rows_on_screen, counted,
            "width {width}: 画面上 {rows_on_screen} 行有内容，模型只数了 {counted} 行——\n\
             折行落点按字符数算的，CJK 每行宽度翻倍，计数侧与画面脱节"
        );
    }
}

/// T23：emoji 与组合符同样按显示格宽处理。
///
/// emoji 是 2 格，组合附加符号是 0 格。手搓码位段表对这两类的判断依赖
/// 「终端按 CJK wide 渲染」这一前提；unicode-width 的口径是确定的，宽度
/// 与断行因此在两种终端配置下一致。
#[test]
fn emoji_and_combining_marks_count_as_display_cells() {
    let body = format!("{} {}", "🙂".repeat(30), "é".repeat(30));
    for width in [20u16, 40] {
        let mut app = history(vec![user_msg(&body)]);
        sync(&mut app, width, 30);
        let drawn = screen_at(&app, width, 30);
        let rows = drawn
            .lines()
            .filter(|l| l.contains('🙂') || l.contains('\u{301}'))
            .count();
        assert_eq!(
            rows,
            app.viewport().total(),
            "width {width}: emoji/组合符行的实际行数与模型计数脱节"
        );
    }
}
