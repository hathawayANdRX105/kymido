//! diff — route T27：edit 类工具结果的统一 diff 渲染契约。头号不变式仍是
//! `rows == lines.len()`；判据宁严勿宽，误判比漏判严重。

use ratatui::{Terminal, backend::TestBackend};

use kymido_tui::app::App;
use kymido_tui::ui;
use kymido_tui::ui::diff;
use web_state::types::{ChatMessage, MessagePart};

fn tool_detail(detail: &str) -> ChatMessage {
    ChatMessage {
        id: "m-diff".to_string(),
        role: "assistant".to_string(),
        content: String::new(),
        reasoning: String::new(),
        tool_calls: vec![],
        parts: vec![MessagePart::Tool(web_state::types::ToolCall {
            id: "t-1".to_string(),
            title: "edit config.toml".to_string(),
            kind: "edit".to_string(),
            summary: String::new(),
            detail: detail.to_string(),
            status: "success".to_string(),
        })],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

fn history(msgs: Vec<ChatMessage>) -> App {
    let mut app = App::new();
    app.switch_session("s-diff", msgs);
    app
}

fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
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

const SAMPLE: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,7 +10,9 @@ fn main() {\n fn main() {\n-    let old = 1;\n+    let new = 2;\n+    let extra = 3;\n     println!(\"hi\");\n }";

/// 头号不变式：计数侧与物化侧逐条相等。
#[test]
fn diff_rows_match_lines() {
    let samples = [
        SAMPLE.to_string(),
        "@@ -1,3 +1,3 @@\n-a\n+b".to_string(),
        "not a diff at all\nplain lines".to_string(),
        String::new(),
    ];
    for text in samples {
        for width in [12usize, 44, 100] {
            let rows = diff::rows(&text, width);
            let lines = diff::lines(&text, width);
            assert_eq!(
                rows,
                lines.len(),
                "{width}cols: rows {rows} != lines {} —— count/render 分叉",
                lines.len()
            );
        }
    }
}

/// ± 标记画对方向：`+` 行在画面上带 `+` 且整行可辨识为新增，`-` 同理。
/// 画反意味着新增行渲染成删除色（或装订线错位），review diff 时会误读。
#[test]
fn diff_marks_add_and_delete() {
    assert!(
        diff::looks_like_diff(SAMPLE),
        "成对文件头 + hunk 头要判成 diff"
    );
    let lines = diff::lines(SAMPLE, 100);
    let joined: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect();
    let del = joined.iter().filter(|l| l.contains("-    let old")).count();
    let add = joined.iter().filter(|l| l.contains("+    let new")).count();
    assert_eq!(del, 1, "删除行必须恰好出现一次且带 -");
    assert_eq!(add, 1, "新增行必须恰好出现一次且带 +");
    // 内容本身不许被吃掉。
    assert!(
        joined.iter().any(|l| l.contains("println")),
        "上下文行必须保留"
    );
}

/// 判据宁严勿宽的另一半：`--- ` 单独出现（YAML 文档分隔、表格分隔线）
/// **不**判成 diff。
#[test]
fn non_diff_detail_uses_text_path() {
    let yaml = "key: value\n--- \nnext: doc";
    assert!(!diff::looks_like_diff(yaml), "单独 --- 不得判成 diff");
    // 普通工具输出走原路径：行数 == 纯文本全显行数。
    let plain = "output line one\noutput line two";
    let rows = diff::rows(plain, 80);
    let lines = diff::lines(plain, 80);
    assert_eq!(rows, 2, "普通输出按纯文本全显");
    assert_eq!(rows, lines.len());
}

/// 头部形态像 diff 但 diffy 拒收 → 回落纯文本：内容一行不丢、不 panic。
#[test]
fn diff_falls_back_on_parse_error() {
    let broken = "--- a/f\n+++ b/f\n@@ -1,1 +1,1 @@\n这是畸形 hunk：行数对不上";
    assert!(diff::looks_like_diff(broken), "头部形态像 diff");
    let rows = diff::rows(broken, 80);
    let lines = diff::lines(broken, 80);
    assert_eq!(rows, lines.len(), "回落路径的 count/render 分叉");
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(
        joined.contains("畸形 hunk"),
        "回落路径不许吞内容:\n{joined}"
    );
}

/// 空隙标记：相邻 hunk 隔了 > 3 行删除侧上下文时插 `… N lines skipped`，
/// 不许几千行上下文刷屏；间隔小（≤3）则不插。
#[test]
fn diff_gap_marker_inserted() {
    // 上下文 11 行（> 3）→ 插标记。头计数必须与实际行数精确一致，
    // 否则 diffy 拒收、走纯文本回落（那是另一条测试的事）：
    // 旧侧 = 1 删 + 11 上下文 = 12 行，新侧 = 1 增 + 11 上下文 = 12 行。
    let far = format!(
        "@@ -1,12 +1,12 @@\n-a\n+b\n{}\n@@ -20,1 +20,1 @@\n-c\n+d",
        (1..=11)
            .map(|i| format!(" ctx{i}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let lines = diff::lines(&far, 80);
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(
        joined.contains("lines skipped"),
        "大间隔必须插空隙标记\nlooks_like_diff={}\nlines={}\n{joined}",
        diff::looks_like_diff(&far),
        lines.len()
    );
    // 空隙 = 20 -（hunk1 起点 1 + 旧侧行数 12 - 1）= 8。
    assert!(
        joined.contains("8 lines skipped"),
        "空隙行数必须精确（20 - 上一 hunk 结束 12）:\n{joined}"
    );

    // 上下文 2 行（≤ 3）→ 不插。头计数对齐（hunk1 旧侧 1 删 + 2 上下文
    // = 3 行；hunk2 单删单增 = 1 行），间隔 = 5 -（1 + 3 - 1）= 2。
    let near = "@@ -1,3 +1,3 @@\n-a\n+b\n ctx1\n ctx2\n@@ -5,1 +5,1 @@\n-c\n+d";
    let lines = diff::lines(near, 80);
    assert_eq!(
        lines.len(),
        6,
        "必须走 diff 路径：4 + 2 行渲染，头不当内容、行不折双\n{lines:?}"
    );
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(
        !joined.contains("lines skipped"),
        "小间隔不许插空隙标记:\n{joined}"
    );
}

/// 行数上限：超限截断 + 显式提示，不许把整屏刷成几千行 diff。
#[test]
fn diff_row_cap() {
    let body: String = (0..500)
        .map(|i| format!("+    inserted line {i:03}\n"))
        .collect();
    let text = format!("--- a/f\n+++ b/f\n@@ -0,0 +1,500 @@\n{body}");
    let rows = diff::rows(&text, 80);
    let lines = diff::lines(&text, 80);
    assert_eq!(rows, lines.len(), "超限 diff 的 count/render 分叉");
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(
        rows < 250,
        "渲染 {rows} 行——200 逻辑行上限 + 显式提示，物理行不该翻倍:\n{joined}"
    );
    let joined: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();
    assert!(
        joined.contains("diff truncated"),
        "截断必须显式提示:\n{joined}"
    );
}

/// 端到端：edit 卡在整屏里真的渲染出装订线——`rows/lines` 对偶扩展到
/// `tool_card` 级（diff 分支下 `tool_card::rows` 与 `lines` 恒等）。
#[test]
fn tool_card_rows_match_lines_with_diff() {
    let tc = tool_detail(SAMPLE);
    for width in [44usize, 100] {
        let lines = ui::tool_card::lines(
            match &tc.parts[0] {
                MessagePart::Tool(t) => t,
                _ => unreachable!(),
            },
            ui::fold::Fold::Expanded,
            width,
        );
        let rows = ui::tool_card::rows(
            match &tc.parts[0] {
                MessagePart::Tool(t) => t,
                _ => unreachable!(),
            },
            ui::fold::Fold::Expanded,
            width,
        );
        assert_eq!(
            rows,
            lines.len(),
            "width {width}: tool_card::rows {rows} != lines {} ——diff 分支下对偶分叉",
            lines.len()
        );
    }
    // 整屏证据：装订线（`--- a/` 文件头）真实出现在渲染帧里。
    let app = history(vec![tc]);
    let text = screen(&app);
    assert!(text.contains("--- a/"), "diff 装订线没上屏:\n{text}");
}
