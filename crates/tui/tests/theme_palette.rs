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
//! 主题是进程级槽（`theme::set_active`），每个用例自己设回起点；但 libtest
//! 并行跑用例时「自己设的值」会被别的用例中途改掉（CI 实测 `--test-threads`
//! 2→4 后 `cancel must not switch: left Light right Dark` 稳定复现），所以
//! 碰主题槽的用例一律先拿 `theme_guard()` 串行化。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kymido_tui::app::App;
use kymido_tui::theme::{self, Scheme};
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
                // 宽 ≥2 时连带吃掉 (width-1) 个续格；`.max(1)` 防零宽下溢。
                x += cell.cell_width().max(1) - 1;
            }
            // 无条件推进：1 宽格上 `cell_width()-1 == 0`，若只在 else 分支
            // 推进，游标永远不动 = 死循环（2026-09-27 CI 首跑 hang 实证）。
            x += 1;
        }
        out.push('\n');
    }
    out
}

/// 画一帧 enhanced 外壳并取文本。
fn draw_text(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).expect("terminal");
    terminal
        .draw(|frame| kymido_tui::ui::draw(frame, app))
        .expect("draw must fit without panicking");
    buffer_text(terminal.backend().buffer())
}

/// 画一帧并取单元格样式串（fg + modifier）——切主题的「画面变了」只看这个。
fn draw_styles(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).expect("terminal");
    terminal
        .draw(|frame| kymido_tui::ui::draw(frame, app))
        .expect("draw must fit without panicking");
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if let Some(cell) = buf.cell((x, y))
                && cell.symbol() != " "
            {
                out.push_str(&format!("{:?}:{:?};", cell.fg, cell.modifier));
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

/// 进程级主题槽的用例互斥：碰 `theme::set_active` 的用例开头拿锁，避免
/// A 的切换撞进 B 的断言窗口（见文件头注释）。毒锁用 `into_inner` 吃掉
/// ——一个用例失败不连坐其余用例。
fn theme_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    let _guard = theme_guard();
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
    let _guard = theme_guard();
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
    let _guard = theme_guard();
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
    let _guard = theme_guard();
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
    let _guard = theme_guard();
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
/// T19：新增的 `Border` / `Success` / `Warn` 三个角色必须两档都给色，且
/// 每档取不同色。
///
/// 缺臂是编译期错误（`Scheme::color` 的 match 穷尽），所以这条真正防的是
/// 另两种事故：新角色两档**同色**——在某一档上直接不可读（`Dim` 档踩过
/// DarkGray 在亮底看不见的老坑）；以及某一档随手抄了另一档的值。
#[test]
fn new_roles_differ_between_schemes() {
    theme::set_active(Scheme::Dark);
    let dark = (theme::border().fg, theme::success().fg, theme::warn().fg);
    theme::set_active(Scheme::Light);
    let light = (theme::border().fg, theme::success().fg, theme::warn().fg);
    theme::set_active(Scheme::Dark);

    for (role, dark_fg, light_fg) in [
        ("border", dark.0, light.0),
        ("success", dark.1, light.1),
        ("warn", dark.2, light.2),
    ] {
        assert!(dark_fg.is_some(), "{role} must have a colour in dark");
        assert!(light_fg.is_some(), "{role} must have a colour in light");
        assert_ne!(
            dark_fg, light_fg,
            "{role} must differ between schemes or it is unreadable on one"
        );
    }
}
