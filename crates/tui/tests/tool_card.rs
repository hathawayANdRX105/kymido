//! tool_card — route §4 T3：一次 `ToolCall→ToolStart→ToolResult` 序列折叠
//! 成一张卡（不是三行）、`ToolResult` 后卡片不卡 running；长结果折叠成
//! 「头尾预览 + 隐藏行数」，一个键全部展开/折叠。TestBackend 渲染断言
//! （取样法同 `tests/layout.rs`）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use kymido_tui::app::App;
use kymido_tui::ui;
use web_state::types::ToolCall;
use web_state::ui_state::AgentEvent;

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

/// 卡头计数（`▸` 只出现在工具卡卡头）。
fn cards(text: &str) -> usize {
    text.matches('▸').count()
}

#[test]
fn tool_sequence_folds_into_one_card() {
    let mut app = App::new();
    app.apply_event(&AgentEvent::ToolCall {
        id: "call-1".to_string(),
        name: "run_bash".to_string(),
        args: serde_json::json!({ "command": "ls -la" }),
    });
    // 序列中途：卡已出现且是 running——不是「每个事件一行/一卡」。
    let mid = screen(&app);
    assert_eq!(cards(&mid), 1, "ToolCall 只该出一张卡:\n{mid}");
    assert!(mid.contains("running"), "卡头要带 running 三态:\n{mid}");

    app.apply_event(&AgentEvent::ToolStart {
        id: "call-1".to_string(),
    });
    let started = screen(&app);
    assert_eq!(cards(&started), 1, "ToolStart 不许渲出第二张卡:\n{started}");
    assert!(
        started.contains("running"),
        "ToolStart 后仍在同张卡上跑:\n{started}"
    );

    app.apply_event(&AgentEvent::ToolResult {
        id: "call-1".to_string(),
        name: "run_bash".to_string(),
        result: "file1\nfile2".to_string(),
    });
    let done = screen(&app);
    assert_eq!(
        cards(&done),
        1,
        "一次 ToolCall→ToolStart→ToolResult 只该有一张卡:\n{done}"
    );
    assert!(done.contains("done"), "ToolResult 后卡头翻到 done:\n{done}");
    assert!(
        !done.contains("running"),
        "ToolResult 后卡片不能卡在 running:\n{done}"
    );
    assert!(done.contains("file1"), "结果正文要显示:\n{done}");

    // 同 id 重复 ToolCall → 新卡（不合并，route §3 边界）。
    app.apply_event(&AgentEvent::ToolCall {
        id: "call-1".to_string(),
        name: "run_bash".to_string(),
        args: serde_json::json!({ "command": "pwd" }),
    });
    let second = screen(&app);
    assert_eq!(cards(&second), 2, "同 id 重复 ToolCall 应是新卡:\n{second}");
}

#[test]
fn long_tool_result_collapses_to_header() {
    let mut app = App::new();
    assert!(!app.tools_expanded(), "默认折叠（展开键尚未按下）");
    let result = (0..20)
        .map(|i| format!("line-{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.apply_event(&AgentEvent::ToolCall {
        id: "call-long".to_string(),
        name: "run_bash".to_string(),
        args: serde_json::json!({ "command": "seq 1 20" }),
    });
    app.apply_event(&AgentEvent::ToolResult {
        id: "call-long".to_string(),
        name: "run_bash".to_string(),
        result,
    });

    let collapsed = screen(&app);
    assert!(collapsed.contains('▸'), "卡头在:\n{collapsed}");
    assert!(
        collapsed.contains("line-00"),
        "头预览保留首行:\n{collapsed}"
    );
    assert!(
        collapsed.contains("line-19"),
        "尾预览保留末行:\n{collapsed}"
    );
    assert!(
        !collapsed.contains("line-10"),
        "长结果默认折叠，中段不许撑爆 dock:\n{collapsed}"
    );
    assert!(
        collapsed.contains("15 lines hidden"),
        "折叠要报隐藏行数（20 行 - 头 3 - 尾 2）:\n{collapsed}"
    );

    // 一个键全部展开（route §3：一个键展开/折叠，Tab 不进 composer）。
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(app.tools_expanded(), "该键要翻转展开状态");
    let expanded = screen(&app);
    assert!(expanded.contains("line-10"), "展开后中段可见:\n{expanded}");
    assert!(
        !expanded.contains("lines hidden"),
        "展开后不再报隐藏行数:\n{expanded}"
    );

    // 同一键再按 = 全部折回。
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert!(!app.tools_expanded(), "再按一次折回折叠态");
}
// ——— T22：字形双通道 — 辅助与测试 —

/// 直接构造一张卡（绕开事件流：kind / status 原样给，任意 kind 与三态
/// 都能直接钉）。
fn tc(kind: &str, status: &str, title: &str, detail: &str) -> ToolCall {
    ToolCall {
        id: "t-1".to_string(),
        title: title.to_string(),
        kind: kind.to_string(),
        summary: String::new(),
        detail: detail.to_string(),
        status: status.to_string(),
    }
}

/// 卡头（卡的第一行）文本（剥掉样式：钉的是字形本身，不是颜色）。
fn header_of(kind: &str, status: &str) -> String {
    let lines = ui::tool_card::lines(&tc(kind, status, "cmd", ""), false, 80);
    lines
        .first()
        .and_then(|line| line.spans.first())
        .map(|span| span.content.to_string())
        .unwrap_or_default()
}

/// T22：三态必须无颜色可辨（catches colour-only state：状态原来只靠卡头
/// 颜色区分（running=brand / done=dim / failed=danger）——NO_COLOR 终端、
/// 或分不出红绿的读者，三态视觉上无法区分，且窄列下尾部文案被裁出屏后
/// 状态信息归零；修法是把形状放进状态位本身）。
#[test]
fn status_glyph_distinguishes_states_without_color() {
    let running = header_of("bash", "running");
    let done = header_of("bash", "success");
    let failed = header_of("bash", "error");
    let unknown = header_of("bash", "weird");

    assert!(running.contains('~'), "running 要有自己的形状:\n{running}");
    assert!(done.contains('✓'), "done 要有自己的形状:\n{done}");
    assert!(failed.contains('✗'), "failed 要有自己的形状:\n{failed}");
    assert!(
        unknown.contains('?'),
        "未知状态标 ?（不编造状态）:\n{unknown}"
    );
    // 无颜色时三态卡头两两文本可分——两个态共用同一卡头即 NO_COLOR 下不可辨。
    assert_ne!(running, done, "running/done 卡头要文本可分");
    assert_ne!(done, failed, "done/failed 卡头要文本可分");
    assert_ne!(running, failed, "running/failed 卡头要文本可分");
}

/// T22：未知 kind 必须回落到既定字形（catches a match arm miss：新工具
/// kind 或历史落库的未识别字符串没有表项——字形位渲染成空串或 panic；
/// 契约是任意 kind 回落通用品形 ⚙，且已知 kind 不退回兜底）。
#[test]
fn unknown_kind_falls_back() {
    for kind in ["tool", "mystery_tool", "", "BASH"] {
        let header = header_of(kind, "success");
        assert!(
            header.contains('⚙'),
            "kind {kind:?} 要回落到通用品形:\n{header}"
        );
        if !kind.is_empty() {
            assert!(header.contains(kind), "kind 文本本身仍要显示:\n{header}");
        }
    }
    // 已知 kind 各走自己的字形（表项逐条钉，一条回归一条红）。
    let table = [
        ("bash", '$'),
        ("job", '&'),
        ("terminal", '⌗'),
        ("read", '≡'),
        ("write", '⇓'),
        ("edit", '✎'),
        ("delete", '⌫'),
        ("grep", '⌕'),
        ("glob", '⌕'),
        ("subagent", '↗'),
    ];
    for (kind, glyph) in table {
        let header = header_of(kind, "running");
        assert!(
            header.contains(glyph),
            "kind {kind} 要用自己的字形 {glyph}:\n{header}"
        );
    }
}

/// T22：字形不得改变行数（catches a multi-cell glyph：宽字形让 `lines`
/// 多出行而 `rows` 仍按旧口径计数——窗口化 transcript 总行数与物化行数
/// 分叉，滚动与画面失同步；修法是全单格字形 + 计数/物化同源）。
#[test]
fn glyphs_do_not_change_row_count() {
    let kinds = [
        "bash",
        "job",
        "terminal",
        "read",
        "write",
        "edit",
        "delete",
        "grep",
        "glob",
        "subagent",
        "tool",
        "mystery_kind",
    ];
    let statuses = ["running", "success", "error", "weird"];
    let details: Vec<String> = vec![
        String::new(),
        "file1\nfile2".to_string(),
        (0..20)
            .map(|i| format!("line-{i:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ];
    for kind in kinds {
        for status in statuses {
            for detail in &details {
                let card = tc(kind, status, "cmd title", detail);
                for (expanded, width) in [
                    (false, 80usize),
                    (true, 80),
                    (false, 44),
                    (true, 44),
                    (false, 12),
                    (true, 12),
                ] {
                    let lines = ui::tool_card::lines(&card, expanded, width);
                    assert_eq!(
                        ui::tool_card::rows(&card, expanded, width),
                        lines.len(),
                        "kind {kind} status {status} expanded {expanded} width {width}: \
                         rows/lines 分叉"
                    );
                }
            }
        }
    }
}
