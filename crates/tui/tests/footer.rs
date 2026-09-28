//! footer — route §4 T3：footer 只许出现已核字段——开工先核结论（`stats.
//! summary` 与消息 usage 均无 per-context 占用数据）→ 空闲段只有
//! `model · run 状态`，任何 context 百分比 / token / 计费字段都是编造，
//! 必须让本测试变红。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use kymido_tui::app::App;
use kymido_tui::ui;

/// TestBackend 缓冲 → 逐行文本（同 dsh `tests/chat_flow.rs` 的取样法）。
fn buffer_text(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
            }
        }
        out.push('\n');
    }
    out
}

/// 渲染一帧 80×24 并取整屏文本。
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw must fit without panicking");
    buffer_text(terminal.backend().buffer())
}

/// 含 `needle` 的那一行（footer 是唯一带 model 的行）。
fn footer_row<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing `{needle}` in:\n{text}"))
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        app.type_char(c);
    }
}

#[test]
fn footer_shows_only_verified_fields() {
    let mut app = App::new();
    app.set_model("test-model-x");

    // 运行中：`model · 耗时 · Esc 中断提示`（route §3 footer 契约）。
    type_str(&mut app, "hello");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        kymido_tui::app::KeyAction::None
    );
    assert_eq!(app.next_to_send().as_deref(), Some("hello"));
    let running = screen(&app);
    let running_footer = footer_row(&running, "test-model-x");
    assert!(
        running_footer.contains("ms"),
        "运行中出耗时段:\n{running_footer}"
    );
    assert!(
        running_footer.contains("esc abort"),
        "运行中出 Esc 中断提示:\n{running_footer}"
    );

    // 空闲：`model · run 状态`；未核字段（context 占用/百分比/token/费用）
    // 一律不许出现——出现即「footer 编造数据」的 bug。
    app.note_turn_end();
    app.set_in_flight(0);
    let idle = screen(&app);
    let idle_footer = footer_row(&idle, "test-model-x");
    assert!(
        idle_footer.contains("idle"),
        "空闲出 run 状态:\n{idle_footer}"
    );
    for forbidden in ["%", "context", "token", "$"] {
        assert!(
            !idle.contains(forbidden),
            "footer 只许出已核字段，`{forbidden}` 是编造数据:\n{idle}"
        );
    }

    // 已核字段换档：`stats.summary` 的半开 run 计数 → `{n} in flight`。
    app.set_in_flight(1);
    let inflight = screen(&app);
    let inflight_footer = footer_row(&inflight, "test-model-x");
    assert!(
        inflight_footer.contains("1 in flight"),
        "run 状态来自 stats.summary:\n{inflight_footer}"
    );
    assert!(!inflight_footer.contains('%'), "照样不许有百分比");
}

/// 含 `needle` 的那一行（取首条；找不到即 panic——活动行按全屏唯一子串
/// “running” / “○ idle” 定位）。
fn active_row<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing `{needle}` in:\n{text}"))
}

/// P1 braille 转盘：运行中时 dock 活动行显示 [`SPINNER_FRAMES`] 起步帧
/// （`⠋ running`，帧 = 0），空闲退回 `○ idle` 且全屏不许出现任何 braille 帧。
/// 两个方向都钉死可观测渲染态——只查「有某个 glyph」不够，必须钉到帧序与
/// 运行态的对应。
#[test]
fn dock_running_line_shows_braille_spinner() {
    let mut app = App::new();
    app.set_model("test-model-x");

    // 运行中：提交 prompt → running=true、spinner 起步帧 = 0（`take_prompt`
    // 复位，测试不驱动 `tick_spinner`，帧定在 0 可复现）。活动行 = `⠋ running`。
    type_str(&mut app, "hello");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        kymido_tui::app::KeyAction::None
    );
    assert_eq!(app.next_to_send().as_deref(), Some("hello"));
    assert!(app.is_running(), "prompt 已出站，应为 running");
    let running = screen(&app);
    let active = active_row(&running, "running");
    assert!(
        active.contains(kymido_tui::app::SPINNER_FRAMES[0]),
        "运行中活动行出 braille 起步帧:\n{active}"
    );
    assert!(
        !running.contains("○ idle"),
        "运行中不许出现 idle 标记:\n{running}"
    );

    // 空闲：TurnEnd → 活动行退回 `○ idle`，全屏不许出现任何 braille 帧
    // （「没在跑却转」的 bug 在此被红）。
    app.note_turn_end();
    app.set_in_flight(0);
    let idle = screen(&app);
    assert!(idle.contains("○ idle"), "空闲活动行出 idle 标记:\n{idle}");
    for frame in kymido_tui::app::SPINNER_FRAMES {
        assert!(
            !idle.contains(frame),
            "空闲时 braille 帧 `{frame}` 不许出现:\n{idle}"
        );
    }
}
