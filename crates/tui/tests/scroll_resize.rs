//! scroll_resize — route §3 T6「滚动位置在 resize 后 clamp 不越界」：视口
//! 变高 / 内容骤减后 offset 夹紧不越界、渲染不 panic 不白屏（bug：resize
//! 越界 panic / 白屏），以及内容总行数随列宽重数——窄列多断行，窗口拿旧
//! 计数就会错位。
//!
//! 模型侧不碰真终端（`ScrollModel::sync` 直接喂几何），渲染侧走 TestBackend。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use kymido_tui::app::App;
use kymido_tui::scroll::ScrollModel;
use kymido_tui::ui;
use web_state::types::{ChatMessage, MessagePart};
use web_state::ui_state::AgentEvent;

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

/// TestBackend 缓冲 → 逐行文本（同 `tests/layout.rs` 的取样法）。
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

/// 首个包含 `needle` 的行号（0-based）。
fn row_of(text: &str, needle: &str) -> Option<usize> {
    text.lines().position(|line| line.contains(needle))
}

/// 画一帧取整屏文本（不喂几何——喂几何走 [`sync`]）。
fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw must fit without panicking");
    buffer_text(terminal.backend().buffer())
}

/// 每帧几何同步（事件循环 draw 前的那一步，测试走同一入口）。
fn sync(app: &mut App, width: u16, height: u16) {
    ui::sync_viewport(app, Rect::new(0, 0, width, height));
}

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

/// 60 条单行历史（80×24 下视口 20 行、max = 40）。
fn scrollable_app(lines: usize) -> App {
    let mut app = App::new();
    let history = (0..lines)
        .map(|i| user_msg(&format!("mid-{i:03}")))
        .collect();
    app.start_session("s-1", history);
    app
}

/// 脱钩后屏幕变高：视口吃掉更多内容 → max 骤缩，offset 必须 clamp 进新
/// 边界（bug：旧 offset > 新 max → 越界 / 渲染错位），且窗口从新顶开始。
#[test]
fn viewport_offset_clamps_when_viewport_grows() {
    let mut app = scrollable_app(60);
    sync(&mut app, 80, 24);
    app.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(app.viewport().offset(), 21, "脱钩在 80×24 的位置");
    assert!(!app.viewport().is_following());

    // 80×60：transcript 视口 56 行 → max = 60 − 56 = 4（原 offset 21 越界）。
    sync(&mut app, 80, 60);
    assert_eq!(app.viewport().max_offset(), 4);
    assert_eq!(
        app.viewport().offset(),
        4,
        "resize 后 offset clamp 进新边界，不越界"
    );
    assert!(app.viewport().offset() <= app.viewport().max_offset());

    // 渲染不 panic，且窗口从 clamp 后的新顶开始（mid-004 在第 0 行）。
    let text = screen(&app, 80, 60);
    assert_eq!(row_of(&text, "mid-004"), Some(0), "窗口按 clamp 后的顶渲染");
    assert!(
        row_of(&text, "history").is_some(),
        "不许白屏：dock 按键提示还在:\n{text}"
    );
}

/// 模型侧的几何边界矩阵：内容骤减、视口骤增、0 高/0 内容——一律夹紧到
/// 合法区间，不 panic、不下溢。
#[test]
fn model_offset_stays_clamped_across_geometry_jumps() {
    let mut model = ScrollModel::new();
    model.sync(500, 20);
    assert_eq!(model.offset(), 480, "跟尾钉底");
    model.page_up();
    assert!(!model.is_following());

    // 内容骤减（500 → 30 行）：clamp 到新 max = 10。
    model.sync(30, 20);
    assert_eq!(model.offset(), 10, "内容骤减 clamp 到新底");
    assert!(model.offset() <= model.max_offset());

    // 视口骤增（20 → 1000 行，比内容还高）：max = 0。
    model.sync(30, 1000);
    assert_eq!(model.offset(), 0, "视口高于内容时回顶（= 底）");
    assert_eq!(model.view_top(30, 1000), 0);

    // 0 高 / 0 内容：恒 0，不 panic、不下溢。
    model.sync(0, 0);
    assert_eq!(model.offset(), 0);
    assert_eq!(model.view_top(0, 0), 0);

    // 脱钩态 + 视口骤增同样夹紧。
    let mut detached = ScrollModel::new();
    detached.sync(500, 20);
    detached.page_up();
    detached.sync(30, 1000);
    assert_eq!(detached.offset(), 0, "脱钩位也 clamp 进新边界");
    assert_eq!(detached.view_top(30, 1000), 0);
}

/// 多档 resize × 流式 × 翻页循环：每一步都 clamp 不越界、不 panic，
/// dock 恒压底（不白屏、不塌布局）。
#[test]
fn resize_streaming_cycle_never_panics_or_blanks() {
    let mut app = scrollable_app(30);
    let mut flip = false;
    for (width, height) in [(44u16, 12u16), (100, 40), (60, 13), (80, 24), (44, 20)] {
        sync(&mut app, width, height);
        app.apply_event(&AgentEvent::AssistantText {
            delta: "chunk line one\nchunk line two".to_string(),
        });
        flip = !flip;
        let code = if flip {
            KeyCode::PageUp
        } else {
            KeyCode::PageDown
        };
        app.handle_key(key(code, KeyModifiers::NONE));
        sync(&mut app, width, height);
        assert!(
            app.viewport().offset() <= app.viewport().max_offset(),
            "{width}x{height}: sync 后 offset 必须 clamp 进 [0, max]"
        );

        let text = screen(&app, width, height);
        assert_eq!(
            row_of(&text, "history"),
            Some((height - 1) as usize),
            "{width}x{height}: hints 恒在最底一行（不白屏、不塌布局）:\n{text}"
        );
    }
}

/// 内容总行数随列宽重数：同一内容，窄列断行更多 → total 变大；resize 后
/// 拿旧计数开窗就会错位（bug：窗口错位 / 底行被截）。
#[test]
fn transcript_total_recounts_after_width_change() {
    let mut app = App::new();
    let mut history = vec![user_msg(&"x".repeat(60))];
    for i in 0..9 {
        history.push(user_msg(&format!("mid-{i:03}")));
    }
    app.start_session("s-1", history);

    sync(&mut app, 80, 24);
    let wide = app.viewport().total();
    sync(&mut app, 44, 24);
    let narrow = app.viewport().total();

    // 60 字符单词：80 列下 "❯ " + 78 可用宽一行放下（1 行），44 列下
    // 可用 42 列硬切成 2 行；其余 9 条两档都是 1 行。
    assert_eq!(wide, 10, "宽列：同一内容行数少");
    assert_eq!(narrow, 11, "窄列：同一内容多断 1 行");
    assert!(narrow > wide, "总行数必须随列宽重算");
}

// --- T24 滚动条迟滞：决策跟随上一帧档位，单帧不翻转 ---

/// 单行 assistant 消息（无前缀；行数按内容宽断行）。
fn assistant_msg(text: &str) -> ChatMessage {
    ChatMessage {
        id: format!("a-{text}"),
        role: "assistant".to_string(),
        content: text.to_string(),
        reasoning: String::new(),
        tool_calls: vec![],
        parts: vec![],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

/// 边界内容：18 条单行 + 80 字符单词 + 3 条单行——80 列下恰 22 行（= 80×26
/// 的视口高度，放得下），79 列下单词断成 23 行（放不下）。迟滞就活在这
/// 「宽放得下 / 窄放不下」的档位：结果跟随**上一帧**的档位，不按当前帧
/// 数字重算。
fn boundary_app() -> App {
    let mut history: Vec<ChatMessage> = Vec::new();
    for i in 0..18 {
        history.push(user_msg(&format!("top-{i:02}")));
    }
    history.push(assistant_msg(&"w".repeat(80)));
    for i in 0..3 {
        history.push(user_msg(&format!("bot-{i}")));
    }
    let mut app = App::new();
    app.start_session("s-boundary", history);
    app
}

/// 边界「隐藏侧」（bug：可见性拿当前帧数字自定——按窄宽试溢出即预留 →
/// 屏幕重排成 23 行；或计数宽度与画面宽度两套 → `view_top` 跳）：宽放得下、
/// 窄放不下，且上一帧未预留 → 决议留在宽宽，不预留；同几何连两帧画面
/// 逐字节相同（单帧不翻转）。
#[test]
fn scrollbar_hidden_holds_at_fit_boundary() {
    let mut app = boundary_app();
    sync(&mut app, 80, 26);
    assert!(
        !app.scrollbar_visible(),
        "宽（80 列）恰放得下 22 行，上一帧未预留 → 不预留"
    );
    assert_eq!(app.viewport().total(), 22, "按提交的宽宽 80 计数");

    let first = screen(&app, 80, 26);
    assert!(!first.contains('█'), "未预留时不渲染滚动条缩略块:\n{first}");
    assert_eq!(
        row_of(&first, &"w".repeat(80)),
        Some(18),
        "80 字符单词在宽宽下一行放平（未被重排）:\n{first}"
    );

    // 同几何第二帧：决议跟随上一帧档位（未预留），仍不预留，画面逐字节
    // 相同——单帧不翻转。
    sync(&mut app, 80, 26);
    assert!(!app.scrollbar_visible(), "决议跟随上一帧，单帧不翻转");
    assert_eq!(app.viewport().total(), 22, "仍按宽宽计数（未切窄宽）");
    let second = screen(&app, 80, 26);
    assert_eq!(second, first, "同几何连两帧画面必须逐字节相同（不闪烁）");
}

/// 边界「显示侧」（bug：朴素的「按当前帧宽宽重算」实现——宽放得下就退
/// 滚动条 → 屏幕重排 → 闪烁）：同一边界几何，上一帧已预留（窄档位）→
/// 决议按窄宽试溢出 → 仍溢出 → 保持预留；同几何连两帧逐字节相同。
#[test]
fn scrollbar_visible_holds_at_fit_boundary() {
    let mut app = boundary_app();
    // 先建立「上一帧已预留」档位：窄屏 79×26 → 23 行 > 22 行 → 预留
    //（提交宽度 78）。
    sync(&mut app, 79, 26);
    assert!(app.scrollbar_visible(), "窄屏溢出 → 预留");
    assert_eq!(app.viewport().total(), 23, "按提交的窄宽 78 计数");

    // 回到边界几何 80×26：锁存 = 已预留 → 按窄宽 79 试溢出 → 仍溢出 →
    // 保持预留（不是「按宽宽重算 → 放得下 → 退」）。
    sync(&mut app, 80, 26);
    assert!(
        app.scrollbar_visible(),
        "上一帧已预留且窄宽仍溢出 → 保持预留"
    );
    assert_eq!(app.viewport().total(), 23, "按窄宽 79 计数");
    let first = screen(&app, 80, 26);
    assert!(first.contains('█'), "预留列渲染滚动条缩略块:\n{first}");

    // 同几何第二帧：逐字节相同，不翻转。
    sync(&mut app, 80, 26);
    assert!(app.scrollbar_visible(), "决议跟随上一帧，单帧不翻转");
    let second = screen(&app, 80, 26);
    assert_eq!(second, first, "同几何连两帧画面必须逐字节相同（不闪烁）");
}

/// 稳态无抖动（bug：滚动条在稳态帧上同帧自定可见性 → 预留/退留来回
/// 翻转 + 屏幕两次重排 = 闪烁）：内容明确溢出，锁存稳态 true，同几何
/// 连两帧逐字节相同，且 total 按预留宽计数。
#[test]
fn scrollbar_steady_state_does_not_jitter() {
    let mut app = scrollable_app(60);
    sync(&mut app, 80, 24);
    assert!(app.scrollbar_visible(), "明确溢出首帧 → 预留");
    assert_eq!(
        app.viewport().total(),
        60,
        "单行内容行数与宽度无关，按预留宽 79 计数"
    );
    let first = screen(&app, 80, 24);
    assert!(first.contains('█'), "预留列渲染滚动条缩略块:\n{first}");

    // 同几何第二帧：锁存 true → 按窄宽试溢出 → 仍溢出 → 保持预留。
    sync(&mut app, 80, 24);
    assert!(app.scrollbar_visible(), "稳态保持预留");
    assert_eq!(app.viewport().total(), 60);
    let second = screen(&app, 80, 24);
    assert_eq!(second, first, "稳态帧逐字节相同（不抖动）");
}

/// 退化几何（bug：预留列算术在 0 宽 / 0 高屏上下溢，或缩略块公式对
/// 空内容除零）：0 行视口不预留；基宽 2 时预留后内容宽落到 0，计数仍
/// 按提交宽度进行，绘制路径不 panic。
///
/// 内容用无前缀 assistant 消息（「aNNN」4 字符）：行数 = ceil(4 / 提交
/// 宽度)× 60——宽 80 / 宽 2 / 宽 1 三档各不同（60 / 120 / 240），「按
/// 提交宽度计数」才观测得到。
#[test]
fn degenerate_geometry_never_reserves_or_panics() {
    let history: Vec<ChatMessage> = (0..60)
        .map(|i| assistant_msg(&format!("a{i:03}")))
        .collect();
    let mut app = App::new();
    app.start_session("s-degenerate", history);

    // 80×3：dock 占满 3 行 → transcript 0 行 → 不预留；计数照宽宽走。
    sync(&mut app, 80, 3);
    assert!(!app.scrollbar_visible(), "0 行视口 → 不预留");
    assert_eq!(app.viewport().total(), 60, "0 行视口仍按宽宽计数");
    let flat = screen(&app, 80, 3);
    assert!(!flat.contains('█'), "0 行视口不渲染缩略块:\n{flat}");

    // 2×24：基宽 2——预留后内容宽 0；「aNNN」4 字符：宽 2 下 2 行/条
    //（共 120 > 20 行视口 → 预留），宽 1 下 4 行/条。
    sync(&mut app, 2, 24);
    assert!(app.scrollbar_visible(), "120 行 > 20 行视口 → 预留");
    assert_eq!(app.viewport().total(), 240, "按提交的窄宽 1 计数");
    // 内容宽 0 + 预留列 1：绘制路径不 panic。
    let slim = screen(&app, 2, 24);
    assert!(slim.contains('█'), "宽 2 屏缩略块仍渲染在预留列:\n{slim}");
}
