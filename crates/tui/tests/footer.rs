//! footer — route §4 T3：footer 只许出现已核字段——开工先核结论（`stats.
//! summary` 与消息 usage 均无 per-context 占用数据）→ 空闲段只有
//! `model · run 状态`，任何 context 百分比 / token / 计费字段都是编造，
//! 必须让本测试变红。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use omenic_tui::app::App;
use omenic_tui::ui;

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
        omenic_tui::app::KeyAction::None
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

/// fallback 切走 primary 后，footer 必须报**本轮真正服务的** model。
///
/// 配置里的 `model` 只是 primary；waterfall 切到 fallback 时它没参与本轮，
/// 继续报它就是对着不相干的模型说谎。这条钉住 `TurnEnd` 回填的 `active_model`
/// 优先于配置值，且只有回填过才切换。
#[test]
fn footer_reports_active_model_after_fallback() {
    use web_state::ui_state::AgentEvent;

    let mut app = App::new();
    app.set_model("primary-model");

    // 回填前：报配置的 primary。
    assert_eq!(app.model(), "primary-model", "未回填时用配置的 model");
    assert!(footer_row(&screen(&app), "primary-model").contains("primary-model"));

    // `TurnEnd` 带 active_model（waterfall 已切走）→ 改报实际的。
    app.apply_event(&AgentEvent::TurnEnd {
        stop_reason: "end_turn".into(),
        active_model: Some("fallback-model".into()),
    });
    assert_eq!(
        app.active_model(),
        Some("fallback-model"),
        "TurnEnd 回填的 active_model 可读"
    );
    let after = screen(&app);
    let row = footer_row(&after, "fallback-model");
    assert!(
        row.contains("fallback-model"),
        "footer 报实际服务的 model:\n{row}"
    );
    assert!(
        !row.contains("primary-model"),
        "primary 没服务本轮，不许再出现在 footer:\n{row}"
    );

    // `active_model: None` 的旧事件不得清掉已回填的值——那是「本轮没走
    // waterfall / 后端没报」，不是「回到 primary」。
    app.apply_event(&AgentEvent::TurnEnd {
        stop_reason: "end_turn".into(),
        active_model: None,
    });
    assert_eq!(
        app.model(),
        "fallback-model",
        "None 只表示后端没报，不回落配置值"
    );
}
