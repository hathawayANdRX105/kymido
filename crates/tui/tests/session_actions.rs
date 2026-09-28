//! session_actions — route §4 T16：picker 的 `n` 新建 / `r` 改名 / `d` 删除。
//!
//! 钉的 bug 本体三个（与 T4 `session_picker.rs` 同范式：纯状态机 +
//! `TestBackend` buffer 断言，无真终端、无 daemon——RPC 副作用在
//! `picker_loop`，这里断言的是**动作意图 + 可见反馈**）：
//! 1. **`d` 不二次确认就删**（误删）→ 确认态必须先到、`y` 才产出 `Delete`；
//! 2. **误删活跃会话**（bug 本体）→ 活跃行按 `d` 直接拒绝 + 可见提示，
//!    零删除意图、不进确认态；
//! 3. **失败静默 / 假刷新**（`d` 走了但列表假装成功）→ 提示行必须可见。
//!
//! 另覆盖改名提交/取消、新建在空列表可用（空列表正是新建的主入口场景）。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kymido_tui::ui::session_picker::{PickerAction, PickerItem, PickerMode, PickerState, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, CellWidth};
use web_state::types::{Session, SessionStatus};

/// TestBackend 缓冲 → 逐行文本（照 T4 `session_picker.rs` 的取样法，宽字符
/// 续格按实际显示宽跳过）。
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

fn draw(state: &PickerState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
    terminal
        .draw(|frame| render(frame, state))
        .expect("draw must fit without panicking");
    buffer_text(terminal.backend().buffer())
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn item(id: &str, title: &str) -> PickerItem {
    PickerItem {
        session: Session {
            id: id.to_string(),
            title: title.to_string(),
            last_active: String::new(),
            model: String::new(),
            status: SessionStatus::Idle,
            last_active_epoch: 0,
            parent_id: None,
        },
        badge: None,
    }
}

/// 两个会话、活跃 = `s-alpha` 的起步状态（T16 的标准场景）。
fn state() -> PickerState {
    PickerState::new(
        "",
        vec![item("s-alpha", "alpha notes"), item("s-beta", "beta notes")],
    )
    .with_active("s-alpha")
}

/// `d` 必须先到二次确认态并**显式回显会话名**，`y` 才产出删除意图；`n`/Esc
/// 取消零动作（bug 本体：一次按键就删掉）。
#[test]
fn delete_needs_confirmation_naming_the_session() {
    let mut st = PickerState::new("", vec![item("s-beta", "beta notes")]).with_active("s-alpha");

    assert_eq!(st.handle_key(key(KeyCode::Char('d'))), PickerAction::None);
    match st.mode() {
        PickerMode::ConfirmDelete { id, name } => {
            assert_eq!(id, "s-beta");
            assert_eq!(name, "beta notes");
        }
        other => panic!("d must enter ConfirmDelete, got {other:?}"),
    }

    // 取消两路都零动作。
    assert_eq!(st.handle_key(key(KeyCode::Char('n'))), PickerAction::None);
    assert_eq!(st.mode(), &PickerMode::List);
    st.handle_key(key(KeyCode::Char('d')));
    assert_eq!(st.handle_key(key(KeyCode::Esc)), PickerAction::None);
    assert_eq!(st.mode(), &PickerMode::List);

    // 确认才产出删除意图，且携带正确 id。
    st.handle_key(key(KeyCode::Char('d')));
    assert_eq!(
        st.handle_key(key(KeyCode::Char('y'))),
        PickerAction::Delete {
            id: "s-beta".to_string()
        }
    );
    assert_eq!(st.mode(), &PickerMode::List);

    // 确认态下 Enter 等价于 `y`——但别的键全忽略：模态不许漏泄成选中切走。
    st.handle_key(key(KeyCode::Char('d')));
    assert_eq!(st.handle_key(key(KeyCode::Char('q'))), PickerAction::None);
    assert_eq!(
        st.handle_key(key(KeyCode::Enter)),
        PickerAction::Delete {
            id: "s-beta".to_string()
        }
    );
}

/// 活跃会话按 `d` → 拒绝 + 可见提示，零删除意图、**不进确认态**（bug 本体）。
#[test]
fn active_session_cannot_be_deleted() {
    let mut st = state();
    assert_eq!(st.handle_key(key(KeyCode::Char('d'))), PickerAction::None);
    assert_eq!(st.mode(), &PickerMode::List, "must not enter ConfirmDelete");

    // 拒绝后按 `y` 落在列表态 = 只是过滤词，不是删除意图。
    assert_eq!(
        st.handle_key(key(KeyCode::Char('y'))),
        PickerAction::Refetch("y".to_string())
    );

    let notice = st
        .notice()
        .unwrap_or_else(|| panic!("reject must be visible"));
    assert!(
        notice.contains("alpha notes"),
        "notice must name the active session, got {notice:?}"
    );
    let screen = draw(&st);
    assert!(
        screen.contains("不能删除当前会话"),
        "reject must render on screen, got:\n{screen}"
    );
}

/// `r` 提交产出改名意图（id + 新标题），Esc 取消零改动。
#[test]
fn rename_submits_intent_and_cancel_is_noop() {
    let mut st = state();
    st.handle_key(key(KeyCode::Down)); // 游标落到 s-beta
    assert_eq!(st.handle_key(key(KeyCode::Char('r'))), PickerAction::None);
    match st.mode() {
        PickerMode::Rename { id, buffer } => {
            assert_eq!(id, "s-beta");
            assert_eq!(buffer, "beta notes", "buffer prefills the current title");
        }
        other => panic!("r must enter Rename, got {other:?}"),
    }

    // 编辑键进缓冲，不落到过滤词上（模态盖过一切）。
    for c in "XY".chars() {
        assert_eq!(st.handle_key(key(KeyCode::Char(c))), PickerAction::None);
    }
    assert_eq!(st.filter(), "", "modal keys must not leak into the filter");
    assert_eq!(
        st.handle_key(key(KeyCode::Enter)),
        PickerAction::Rename {
            id: "s-beta".to_string(),
            title: "beta notesXY".to_string(),
        }
    );
    assert_eq!(st.mode(), &PickerMode::List);

    // Esc 取消零改动（不进任何写动作）。
    st.handle_key(key(KeyCode::Char('r')));
    assert_eq!(st.handle_key(key(KeyCode::Char('Z'))), PickerAction::None);
    assert_eq!(st.handle_key(key(KeyCode::Esc)), PickerAction::None);
    assert_eq!(st.mode(), &PickerMode::List);
}

/// 确认态与编辑态都要在屏上可见（bug 本体：模态只有状态没画面 = 用户不知道
/// 自己正在确认什么）。
#[test]
fn confirm_and_edit_modes_render() {
    let mut st = PickerState::new("", vec![item("s-beta", "beta notes")]).with_active("s-alpha");
    st.handle_key(key(KeyCode::Char('d')));
    let screen = draw(&st);
    assert!(
        screen.contains("beta notes"),
        "confirm must name the session:\n{screen}"
    );
    assert!(
        screen.contains("y confirm"),
        "confirm must show the keys:\n{screen}"
    );

    st.handle_key(key(KeyCode::Char('n')));
    st.handle_key(key(KeyCode::Char('r')));
    let screen = draw(&st);
    assert!(
        screen.contains("rename ❯"),
        "edit mode must be visible:\n{screen}"
    );
    assert!(
        screen.contains("beta notes"),
        "edit buffer must echo:\n{screen}"
    );
}

/// `n` 在**空列表**上也可用——空列表正是新建的主入口场景（bug 本体：空列表
/// 时 `n` 无响应，用户卡死没路走）。
#[test]
fn new_works_on_empty_list() {
    let mut st = PickerState::new("", vec![]).with_active("s-alpha");
    let screen = draw(&st);
    assert!(screen.contains("无会话"), "empty list must be visible");
    assert_eq!(st.handle_key(key(KeyCode::Char('n'))), PickerAction::New);
}

/// 写动作的失败反馈必须**在屏上可见**（bug 本体：RPC 失败静默、列表假刷新）。
#[test]
fn failure_notice_is_rendered() {
    let mut st = state();
    st.set_notice("删除失败: daemon unreachable");
    let screen = draw(&st);
    assert!(
        screen.contains("删除失败"),
        "failure must be visible, got:\n{screen}"
    );
    // 提示写完回列表态（不留半个模态卡住用户）。
    assert_eq!(st.mode(), &PickerMode::List);
}

/// 删除后重查会把被摘掉的行带走，游标必须夹回（bug 本体：越界 panic / 空白）。
#[test]
fn clamp_cursor_after_rows_removed() {
    let mut st = state();
    st.handle_key(key(KeyCode::Down)); // cursor = 1
    st.set_items(vec![item("s-alpha", "alpha notes")]);
    st.clamp_cursor();
    assert_eq!(st.cursor(), 0);
    st.set_items(vec![]);
    st.clamp_cursor();
    assert_eq!(st.cursor(), 0);
}
