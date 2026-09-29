//! markdown — route T26：assistant 正文（`MessagePart::Text`）的 markdown
//! 渲染契约。头号不变式是 `rows == lines.len()`：计数侧与物化侧分叉会让
//! 窗口化 `total_lines` 与画面脱节，滚动从此错位。TestBackend 渲染，无真
//! 终端。

use ratatui::{Terminal, backend::TestBackend};

use kymido_tui::app::App;
use kymido_tui::ui;
use kymido_tui::ui::markdown;
use web_state::types::{ChatMessage, MessagePart};

/// 用户消息（markdown 不该碰它——用户输入的 `*` 和 `_` 是字面量）。
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
    app.switch_session("s-md", msgs);
    app
}

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

/// 各形态正文 × 多档列宽：计数侧与物化侧必须逐条相等。
///
/// 空文本、纯空白、纯标题、多层列表、围栏代码块、引用块、分隔线、链接、
/// 表格语法（本批不开 ENABLE_TABLES，应按纯文本穿过）、CJK 混排。宽度取
/// 12 / 44 / 80 三档——窄档逼出硬切分支，宽档逼出多行合并分支。
#[test]
fn markdown_rows_match_lines() {
    let samples: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("blank", "  \n \t ".to_string()),
        (
            "headings",
            "# H1\n## H2\n### H3 tail\nbody after heading".to_string(),
        ),
        (
            "lists",
            "- alpha\n- beta\n  1. one\n  2. two\n3. top level".to_string(),
        ),
        (
            "fence",
            "```rust\nfn main() {\n    println!(\"hi 中文\");\n}\n```".to_string(),
        ),
        ("quote", "> quoted line\n> second\nafter".to_string()),
        ("rule", "above\n\n---\n\nbelow".to_string()),
        (
            "link",
            "see [the docs](https://example.invalid/a?b=c) now".to_string(),
        ),
        (
            "table syntax",
            "| a | b |\n|---|---|\n| 1 | 2 |".to_string(),
        ),
        (
            "cjk mixed",
            "# 中文标题\n这是一段中文正文，用来验证 markdown 行的显示格宽断行。".to_string(),
        ),
    ];
    for (label, text) in samples {
        for width in [12usize, 44, 80] {
            let rows = markdown::rows(&text, width);
            let lines = markdown::lines(&text, width);
            assert_eq!(
                rows,
                lines.len(),
                "{label} @ {width}cols: rows {rows} != lines {} ——计数侧与物化侧分叉，\
                 窗口化 total_lines 与画面脱节",
                lines.len()
            );
        }
    }
}

/// 流式半截代码围栏：未闭合的 ``` 必须按「代码块到 EOF」落行——确定行数、
/// 内容不丢、不 panic、不空转。
#[test]
fn markdown_handles_unterminated_fence() {
    let text = "```rust\nfn main() {\n    let 中文 = \"值\";\n    println!(\"{中文}\");\n";
    let rows = markdown::rows(text, 44);
    let lines = markdown::lines(text, 44);
    assert!(rows > 0, "半截围栏不能归 0 行（内容全丢）");
    assert_eq!(rows, lines.len(), "半截围栏的 count/render 分叉");
    let joined: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect();
    let joined = joined.join("\n");
    assert!(
        joined.contains("println"),
        "半截围栏里的代码必须保留:\n{joined}"
    );
}

/// 流式半截 `**`：pulldown 把未闭合的 emphasis 当字面量走 `Event::Text`，
/// 内容一行都不能少——「等它写完」或整段消失都算丢内容。
#[test]
fn markdown_handles_unterminated_emphasis() {
    let text = "**bold text and 中文 continue\n\nsecond paragraph stays";
    let rows = markdown::rows(text, 44);
    let lines = markdown::lines(text, 44);
    assert_eq!(rows, lines.len(), "半截 emphasis 的 count/render 分叉");
    let joined = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("bold text and"),
        "半截 ** 后面的内容必须保留:\n{joined}"
    );
    assert!(
        joined.contains("second paragraph stays"),
        "后续段落必须保留:\n{joined}"
    );
}

/// 控制字节：markdown 产出的全部文本都过 `transcript::sanitize`——代码块
/// 里的 ESC 序列绝不能原样进 cell（`tests/linear_escape.rs` 钉的同一威胁）。
#[test]
fn markdown_strips_control_bytes() {
    let text = "```\nbefore \u{1b}[31mred\u{1b}[0m after\n```";
    let lines = markdown::lines(text, 80);
    for line in &lines {
        for span in &line.spans {
            let content = span.content.to_string();
            assert!(
                !content.chars().any(|c| c.is_ascii_control() && c != '\n'),
                "控制字节漏进 cell:\n{content:?}"
            );
        }
    }
}

/// 用户消息是字面量：`*bold*` 的星号必须原样在画面上，绝不能被 markdown
/// 吃掉当格式渲染（transcript 的 user 早返回分支不许接 markdown）。
#[test]
fn markdown_user_message_stays_literal() {
    let app = history(vec![user_msg("*bold* and _under_")]);
    let text = screen(&app);
    assert!(
        text.contains("*bold*"),
        "用户消息的星号被吃掉了（走了 markdown）:\n{text}"
    );
    assert!(text.contains("_under_"), "下划线也被吃了:\n{text}");
}

/// 代码块双上限：行数或字节超限必须截断并显式提示，不许把整屏刷成一个
/// 巨型代码块，也不许静默丢内容。
#[test]
fn markdown_code_block_row_cap() {
    let body: String = (0..2000)
        .map(|i| format!("line-{i:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!("```\n{body}\n```");
    let rows = markdown::rows(&text, 80);
    let lines = markdown::lines(&text, 80);
    assert_eq!(rows, lines.len(), "超限代码块的 count/render 分叉");
    // 上限 200 行 + 提示行，绝不允许渲染出 2000 行。
    assert!(rows < 500, "代码块 {rows} 行远超上限——截断没生效");
    let joined = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("code block truncated"),
        "截断必须显式提示，不许静默:\n{joined}"
    );
}

/// 空文本 / 纯空白 → 0 行、空 Vec（不凭空出行）。
#[test]
fn markdown_empty_is_zero_rows() {
    for text in ["", "   ", " \n \t "] {
        assert_eq!(markdown::rows(text, 80), 0, "空文本凭空多行: {text:?}");
        assert!(
            markdown::lines(text, 80).is_empty(),
            "空文本物化出空行: {text:?}"
        );
    }
}

/// 软换行保留为硬换行：含 N 个 `\n` 的正文必须出 N+1 行，且每行内容
/// 分属各行——不许塌成一段。
///
/// 这条钉的是流式稳定性，不是 markdown 教条：assistant 正文逐 delta 累加、
/// 每帧重算，单个 `\n` 在 CommonMark 里是「段落内软换行」、要等空行出现
/// 段落才算闭合。塌成一行意味着视觉行结构在流式期间一直漂移（接
/// markdown 之前它就是 N+1 行），且 `scroll_follow.rs` 的流式追加契约
/// 会红——那边的 delta 正是这种形态。
#[test]
fn markdown_soft_break_keeps_line_structure() {
    let text = "s1\ns2\ns3\ns4\ns5";
    let rows = markdown::rows(text, 44);
    let lines = markdown::lines(text, 44);
    assert_eq!(rows, 5, "4 个换行必须出 5 行（软换行不得塌行）");
    assert_eq!(rows, lines.len(), "count/render 分叉");
    for (i, line) in lines.iter().enumerate() {
        let content: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            content.contains(&format!("s{}", i + 1)),
            "第 {i} 行应是 s{}，实际 {content:?}——行序被打乱或内容塌缩",
            i + 1
        );
    }
}
