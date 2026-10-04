//! hit_routing — 批 A：命中表与点击路由、per-part 三态折叠（route §3 批 A）。
//!
//! 不变式钉的是「命中行号 = 物化行号」「点击 → 折叠循环」「脱钩 overlay 点
//! 回底」三条契约，不实现细节：点击坐标从真实渲染帧里来（`ui::draw` 后
//! `App::hit` 登记的屏幕坐标），行数对偶在 Hidden 分支下仍由
//! `rows == lines.len()` 钉。

use ratatui::{Terminal, backend::TestBackend, layout::Rect};

use kymido_tui::app::App;
use kymido_tui::ui;
use kymido_tui::ui::fold::Fold;
use kymido_tui::ui::hit::HitAction;
use web_state::types::{ChatMessage, MessagePart, ToolCall};

// ——— helpers ———

fn tool_msg(idx: usize, detail: &str) -> ChatMessage {
    ChatMessage {
        id: format!("m-{idx}"),
        role: "assistant".to_string(),
        content: String::new(),
        reasoning: String::new(),
        reasoning_started_ms: None,
        reasoning_ms: None,
        tool_calls: vec![],
        parts: vec![MessagePart::Tool(ToolCall {
            id: format!("t-{idx}"),
            title: "cmd title".to_string(),
            kind: "bash".to_string(),
            summary: String::new(),
            detail: detail.to_string(),
            status: "success".to_string(),
        })],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

fn thinking_msg(idx: usize) -> ChatMessage {
    ChatMessage {
        id: format!("m-t{idx}"),
        role: "assistant".to_string(),
        content: String::new(),
        reasoning: "step a\nstep b\nstep c".to_string(),
        reasoning_started_ms: None,
        reasoning_ms: None,
        tool_calls: vec![],
        parts: vec![MessagePart::Text("answer".to_string())],
        timestamp: String::new(),
        ts_epoch_ms: 0,
        attachments: vec![],
    }
}

fn history(msgs: Vec<ChatMessage>) -> App {
    let mut app = App::new();
    app.switch_session("s-hit", msgs);
    app
}

/// Draw one frame at 80×24 (geometry sync first — same entry the event loop
/// calls) so the HitMap reflects the real rendered cells.
fn draw(app: &App) {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    terminal
        .draw(|frame| ui::draw(frame, app))
        .expect("draw ok");
}

fn sync(app: &mut App, width: u16, height: u16) {
    ui::sync_viewport(app, Rect::new(0, 0, width, height));
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

// ——— tests ———

/// 命中表对 `hit(x,y)` 返回登记的动作；后登记的 overlay 盖过先登记的
/// transcript 行（滚动条/工具卡头行都可能落同一格，overlay 必须赢）。
#[test]
fn hit_map_returns_action_and_overlay_wins() {
    let mut map = ui::hit::HitMap::default();
    map.push(
        2,
        3,
        10,
        1,
        HitAction::ToggleToolCard {
            msg_idx: 0,
            part_idx: 1,
        },
    );
    assert_eq!(
        map.hit(5, 3),
        Some(HitAction::ToggleToolCard {
            msg_idx: 0,
            part_idx: 1
        }),
        "命中区内的点要返回登记的动作"
    );
    assert_eq!(map.hit(5, 4), None, "命中区外的行不命中");
    assert_eq!(map.hit(1, 3), None, "命中区左外的列不命中");
    assert_eq!(map.hit(12, 3), None, "命中区右外的列不命中");
    // 后登记优先：同一格再推一个 ScrollToBottom，命中换成后者。
    map.push(2, 3, 10, 1, HitAction::ScrollToBottom);
    assert_eq!(
        map.hit(5, 3),
        Some(HitAction::ScrollToBottom),
        "后登记的 overlay 必须盖过先登记的 transcript 行"
    );
}

/// 点脱钩 overlay → `viewport.to_end()`（跟尾恢复）。`click` 消费命中返回
/// true；点空白返回 false。overlay 只在脱钩时出现——跟尾态点右下角不该命中。
#[test]
fn click_scroll_to_bottom_overlay_restores_follow() {
    // 建一个会脱钩的会话：内容 > 视口，往上滚即 detached。
    let msgs: Vec<ChatMessage> = (0..40).map(|i| tool_msg(i, "out")).collect();
    let mut app = history(msgs);
    sync(&mut app, 80, 24);
    // 上滚脱钩（PageUp 是公开路由，落到 scroll_lines / follow=false）。
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert!(!app.viewport().is_following(), "上滚应脱钩");
    // 重画登记 overlay 命中。
    draw(&app);
    // 命中表里有 ScrollToBottom 动作——找它的坐标。
    let target = find_hit(&app, |a| matches!(a, HitAction::ScrollToBottom))
        .expect("脱钩时 overlay 命中区必须在");
    assert!(app.click(target.0, target.1), "点 overlay 必须消费");
    assert!(app.viewport().is_following(), "点 overlay 必须恢复跟尾");
    // 跟尾后 overlay 消失（重画不再有该命中）。
    draw(&app);
    assert!(
        find_hit(&app, |a| matches!(a, HitAction::ScrollToBottom)).is_none(),
        "跟尾后 overlay 命中区必须消失"
    );
}

/// 点工具卡头 → 三态循环 Collapsed→Expanded→Hidden→Collapsed；Hidden 渲染成
/// stub（`‹hidden›`），点 stub 回到 Collapsed 自恢复。行数对偶两侧同一
/// `fold_for` 查询，Hidden 分支 rows==lines.len() 由 tool_card 对偶钉兜底。
#[test]
fn click_tool_card_header_cycles_three_state_fold() {
    // 一张长结果卡（折叠态下 head/tail 预览）。
    let detail = (0..12)
        .map(|i| format!("line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = history(vec![tool_msg(0, &detail)]);
    sync(&mut app, 80, 24);
    draw(&app);
    let target = find_hit(&app, |a| matches!(a, HitAction::ToggleToolCard { .. }))
        .expect("工具卡头命中区必须在");
    // 初始 Collapsed（tools_expanded=false 默认）。
    assert_eq!(app.fold_for(0, 0), Fold::Collapsed, "默认折叠");
    assert!(app.click(target.0, target.1), "点卡头必须消费");
    assert_eq!(app.fold_for(0, 0), Fold::Expanded, "Collapsed→Expanded");
    draw(&app);
    let target = find_hit(&app, |a| matches!(a, HitAction::ToggleToolCard { .. }))
        .expect("Expanded 态卡头命中区仍须在");
    assert!(app.click(target.0, target.1));
    assert_eq!(app.fold_for(0, 0), Fold::Hidden, "Expanded→Hidden");
    // Hidden = 1 行 stub（整卡消失会让命中目标本身没了，无法找回）。
    let card = match &app.messages()[0].parts[0] {
        MessagePart::Tool(t) => t,
        _ => unreachable!(),
    };
    assert_eq!(
        ui::tool_card::rows(card, Fold::Hidden, 80),
        1,
        "Hidden 必须恒 1 行 stub"
    );
    assert!(screen(&app).contains("‹hidden›"), "Hidden 要渲出 stub");
    draw(&app);
    let target = find_hit(&app, |a| matches!(a, HitAction::ToggleToolCard { .. }))
        .expect("Hidden stub 命中区必须在（可点回）");
    assert!(app.click(target.0, target.1));
    assert_eq!(
        app.fold_for(0, 0),
        Fold::Collapsed,
        "Hidden→Collapsed 自恢复"
    );
}

/// 点思考块头 → 同一三态循环（reasoning 走 `THINKING_PART` 哨兵 part_idx，
/// 与 Tool part 0..n 不撞）。
#[test]
fn click_thinking_header_cycles_three_state_fold() {
    let mut app = history(vec![thinking_msg(0)]);
    sync(&mut app, 80, 24);
    draw(&app);
    let target = find_hit(&app, |a| matches!(a, HitAction::ToggleThinking { .. }))
        .expect("思考块头命中区必须在");
    assert!(app.click(target.0, target.1));
    assert_eq!(
        app.fold_for(0, usize::MAX),
        Fold::Expanded,
        "思考块 part_idx 哨兵 key 进 fold_map"
    );
    assert!(screen(&app).contains("step a"), "Expanded 思考块要全显");
}

/// 行数对偶：fold 三态下 count/render 不分叉——`total_lines`（计数）与
/// `window` 物化同一份 `fold_for`，Hidden/Expanded/Collapsed 各自钉
/// rows==lines.len()（对偶结构性所在，改 fold 分支不动断行核心）。
#[test]
fn fold_three_states_keep_count_render_duality() {
    let detail = (0..12)
        .map(|i| format!("line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let card = match &tool_msg(0, &detail).parts[0] {
        MessagePart::Tool(t) => t.clone(),
        _ => unreachable!(),
    };
    for fold in [Fold::Collapsed, Fold::Expanded, Fold::Hidden] {
        for width in [80usize, 44, 12] {
            assert_eq!(
                ui::tool_card::rows(&card, fold, width),
                ui::tool_card::lines(&card, fold, width).len(),
                "fold {fold:?} width {width}: tool_card rows/lines 分叉"
            );
        }
    }
    // thinking 同钉。
    let reasoning = (0..12)
        .map(|i| format!("r{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    for fold in [Fold::Collapsed, Fold::Expanded, Fold::Hidden] {
        assert_eq!(
            ui::thinking::rows(&reasoning, fold, 80),
            ui::thinking::lines(&reasoning, fold, 80).len(),
            "fold {fold:?}: thinking rows/lines 分叉"
        );
    }
}

/// 位置索引 fold override 随会话切换作废（`adopt_session` 清 `fold_map`）：
/// 新会话里同 (msg_idx,part_idx) 的 override 不许指到别的消息上。
#[test]
fn fold_override_resets_on_session_switch() {
    let mut app = history(vec![tool_msg(0, "out")]);
    sync(&mut app, 80, 24);
    draw(&app);
    let target = find_hit(&app, |a| matches!(a, HitAction::ToggleToolCard { .. }))
        .expect("卡头命中区必须在");
    app.click(target.0, target.1);
    assert_eq!(app.fold_for(0, 0), Fold::Expanded, "切前 override 已置");
    // 切会话 → 位置 override 作废，回默认。
    app.switch_session("s-other", vec![tool_msg(0, "out")]);
    assert_eq!(
        app.fold_for(0, 0),
        Fold::Collapsed,
        "会话切换后位置 override 必须作废"
    );
}

/// Tab 语义：仍是全局默认开关——无 override 的卡随它 Expanded/Collapsed，
/// 有 override 的卡不动（per-card 优先，route §3 批 A 决策）。
#[test]
fn tab_flips_default_but_per_card_override_wins() {
    let detail = (0..12)
        .map(|i| format!("l{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut app = history(vec![tool_msg(0, &detail), tool_msg(1, "out")]);
    sync(&mut app, 80, 24);
    draw(&app);
    // 点第一张卡成 Expanded（override）。
    let t0 = find_hit(&app, |a| {
        matches!(a, HitAction::ToggleToolCard { msg_idx: 0, .. })
    })
    .expect("卡0头命中区必须在");
    app.click(t0.0, t0.1);
    assert_eq!(app.fold_for(0, 0), Fold::Expanded);
    // Tab 翻默认到 Expanded——两张卡都读 Expanded？不对：卡0有 override。
    app.handle_key(key_tab());
    assert!(app.tools_expanded(), "Tab 应翻全局默认");
    // 卡1 无 override → 跟随新默认。
    assert_eq!(
        app.fold_for(1, 0),
        Fold::Expanded,
        "无 override 的卡跟全局默认"
    );
    // 卡0 有 override → 不动（还是 Expanded，恰与默认相同——区别在再翻一次）。
    app.handle_key(key_tab());
    assert!(!app.tools_expanded());
    assert_eq!(
        app.fold_for(0, 0),
        Fold::Expanded,
        "有 override 的卡不跟默认（仍 Expanded）"
    );
    assert_eq!(
        app.fold_for(1, 0),
        Fold::Collapsed,
        "无 override 的卡跟默认折回"
    );
}

// ——— helpers needing App internals ———

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key_tab() -> KeyEvent {
    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
}

/// 在 `App::hit` 里找第一个满足谓词的命中区坐标（屏幕格）。命中表是渲染
/// 副产物——测试用同一来源取坐标，点击落点与画面同帧。
fn find_hit(app: &App, pred: impl Fn(HitAction) -> bool) -> Option<(u16, u16)> {
    let map = app.hit().borrow();
    (0..80u16)
        .flat_map(|x| (0..24u16).map(move |y| (x, y)))
        .find(|&(x, y)| map.hit(x, y).is_some_and(&pred))
}
