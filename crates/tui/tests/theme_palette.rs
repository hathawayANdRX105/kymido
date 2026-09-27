//! theme_palette — route §4 T17：调色板单表收敛 + 双 scheme 切换。
//!
//! 钉的 bug 本体：
//! 1. **切了主题某组件仍是旧色**（组件自己持了字面色，绕过 `theme.rs`）→
//!    `palette_is_the_only_place_holding_colors` 扫全 `crates/tui/src` 源，
//!    `Color::` 只许出现在 `theme.rs`；
//! 2. **切了不生效 / 切不回来** → `switching_changes_semantic_values` 与
//!    `panel_selects_apply_and_cancel_is_noop`；
//! 3. **切换顺手改了渲染文本**（linear 零 ESC 不变量）→
//!    `switching_does_not_change_rendered_text`。
//!
//! 主题是进程级槽（`theme::set_active`），每个用例自己设回起点，测试间不留
//! 脏状态（并行执行也安全——断言只依赖自己设的值）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use omenic_tui::app::App;
use omenic_tui::theme::{self, Scheme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, CellWidth};

/// TestBackend 缓冲 → 逐行文本（照 T4/T16 的取样法，宽字符续格按显示宽跳过）。
fn buffer_text(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut x = 0;
        while x < buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                out.push_str(cell.symbol());
                x += cell.cell_width().max(1) - 1;
            } else {
                x += 1;
            }
        }
        out.push('\n');
    }
    out
}

/// 画一帧 enhanced 外壳并取文本。
fn draw_text(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).expect("terminal");
    terminal
        .draw(|frame| omenic_tui::ui::draw(frame, app))
        .expect("draw must fit without panicking");
    buffer_text(terminal.backend().buffer())
}

/// 画一帧并取单元格样式串（fg + modifier）——切主题的「画面变了」只看这个。
fn draw_styles(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).expect("terminal");
    terminal
        .draw(|frame| omenic_tui::ui::draw(frame, app))
        .expect("draw must fit without panicking");
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if let Some(cell) = buf.cell((x, y)) {
                if cell.symbol() != " " {
                    out.push_str(&format!("{:?}:{:?};", cell.fg, cell.modifier));
                }
            }
        }
    }
    out
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// 打 `/theme` 两段 Enter 打开面板（照 `slash_palette.rs` 的两段 Enter 口径：
/// 第一段只补全，第二段才执行）。
fn open_theme_panel(app: &mut App) {
    app.type_char('/');
    for c in "theme".chars() {
        app.type_char(c);
    }
    app.handle_key(key(KeyCode::Enter)); // 第一段：补全
    app.handle_key(key(KeyCode::Enter)); // 第二段：执行 → 面板
}

/// 调色板单表收敛：全 `crates/tui/src` 里 `Color::` 只许出现在 `theme.rs`。
///
/// 这是「切主题后某组件仍旧色」的结构性防线——组件一旦敢自己拿 `Color`，
/// 切换就绕开它。扫源码文本（不是运行时），故 `Style::new().fg(Color::X)`
/// 这类绕道同样被逮住；注释与字符串里的提及不算。
#[test]
fn palette_is_the_only_place_holding_colors() {
    let src_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src_root];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).expect("readable src tree");
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("theme.rs") {
                continue; // 单表本家
            }
            let text = std::fs::read_to_string(&path).expect("readable source");
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                if code.contains("Color::") {
                    offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "colour literals must live in theme.rs only (D11 single table); found:\n{}",
        offenders.join("\n")
    );
}

/// 切换生效：三个语义色的 fg 随方案变，切回原样还原（bug 本体：切了不生效 /
/// 切不回来）。
#[test]
fn switching_changes_semantic_values() {
    theme::set_active(Scheme::Dark);
    let dark = (theme::brand().fg, theme::dim().fg, theme::danger().fg);
    theme::set_active(Scheme::Light);
    let light = (theme::brand().fg, theme::dim().fg, theme::danger().fg);
    assert_ne!(
        dark, light,
        "switching scheme must change the semantic colours"
    );
    // 正文仍跟随终端前景色（两档都不着色）。
    assert_eq!(
        theme::base().fg,
        None,
        "body text keeps the terminal foreground"
    );
    // 切回还原。
    theme::set_active(Scheme::Dark);
    assert_eq!(
        (theme::brand().fg, theme::dim().fg, theme::danger().fg),
        dark
    );
}

/// 配置往返：`Scheme::ALL` 每项 `name()` → `from_name()` 回原值；未知名落
/// 默认（不 panic——过期配置不许让 TUI 起不来）。
#[test]
fn scheme_names_round_trip_and_unknown_falls_back() {
    for scheme in Scheme::ALL {
        assert_eq!(Scheme::from_name(scheme.name()), Some(scheme));
    }
    assert_eq!(Scheme::from_name("nope"), None);
    assert_eq!(theme::set_by_name("nope"), Scheme::Dark);
    theme::set_active(Scheme::Dark);
}

/// 切换后 enhanced 帧按新方案重绘（bug 本体：状态改了、画面没变）。
#[test]
fn switching_repaints_enhanced_frame() {
    theme::set_active(Scheme::Dark);
    let app = App::new();
    let dark = draw_styles(&app);
    theme::set_active(Scheme::Light);
    let light = draw_styles(&app);
    assert!(
        !dark.is_empty(),
        "the frame must actually paint something to compare"
    );
    assert_ne!(
        dark, light,
        "switching scheme must change rendered cell styles"
    );
    theme::set_active(Scheme::Dark);
}

/// 切换只改颜色、不改任何字符（linear 零 ESC 不变量的同源口径）。
#[test]
fn switching_does_not_change_rendered_text() {
    theme::set_active(Scheme::Dark);
    let app = App::new();
    let dark = draw_text(&app);
    theme::set_active(Scheme::Light);
    let light = draw_text(&app);
    assert_eq!(
        dark, light,
        "switching scheme must not change a single rendered character"
    );
    theme::set_active(Scheme::Dark);
}

/// `/theme` 面板：候选行 = `Scheme::ALL`、↑↓ 移动、Enter 提交生效、Esc 取消
/// 零改动；面板是模态（普通键不漏进 composer——bug 本体：面板开着还在打字）。
#[test]
fn panel_selects_apply_and_cancel_is_noop() {
    theme::set_active(Scheme::Dark);
    let mut app = App::new();
    assert!(!app.theme_panel_open(), "panel starts closed");

    open_theme_panel(&mut app);
    assert!(app.theme_panel_open(), "/theme must open the scheme panel");
    // 游标落在当前活跃方案上（重开不用重新找位置）。
    assert_eq!(app.theme_scheme_selected(), Some(Scheme::Dark));

    let screen = draw_text(&app);
    for scheme in Scheme::ALL {
        assert!(
            screen.contains(scheme.name()),
            "panel must list {}\n{screen}",
            scheme.name()
        );
    }

    // 模态：普通字符不漏进 composer。
    app.type_char('x');
    assert_eq!(app.input(), "", "modal panel must swallow plain keys");

    // Esc 取消零改动。
    app.handle_key(key(KeyCode::Esc));
    assert!(!app.theme_panel_open());
    assert_eq!(theme::active(), Scheme::Dark, "cancel must not switch");

    // 重开 → ↓ 选到 light → Enter 提交生效。
    open_theme_panel(&mut app);
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.theme_scheme_selected(), Some(Scheme::Light));
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.theme_panel_open(), "apply must close the panel");
    assert_eq!(theme::active(), Scheme::Light, "apply must switch");
    theme::set_active(Scheme::Dark);
}
