//! ui/session_picker — 会话导航 picker（route §3 T4）。
//!
//! 三层切分（route §2 纯函数管线）：[`PickerState`] 是无 IO 纯状态（按键
//! 进去、[`PickerAction`] 出来），[`render`] 把状态画进 ratatui 帧，
//! [`run_picker`] 是真终端驱动（取数 → 画 → 读键 → 按需重查）。
//!
//! 行为契约（route §3 T4，签名不许改）：
//! - `filter` 起步：空词 = `list_sessions` 全量列表；
//! - 输入即按新词 `search_sessions` 过滤（空词回落全量）；
//! - Enter 返回选中 session id；ESC 返回 `None`（调用方保持原会话）；
//! - 列表为空 → 显示「无会话」单行，Enter 不选中、不崩；
//! - 每行渲染 `runs_for_session` 最新 run 的 Active/Aborted 徽章；
//! - 选中后的历史回填走 [`load_history`]（`load_messages`），回填进
//!   [`crate::app::App::switch_session`] 时一并设滚动边界。
//!
//! T16 会话管理（route §3 T16）追加：筛选行/列表之外多三态——
//! - `n` 新建（空列表也可用——空列表正是新建的主入口场景）；
//! - `r` 行内改名（编辑缓冲在 [`PickerMode::Rename`]，提交产出
//!   [`PickerAction::Rename`]，Esc 取消零改动）；
//! - `d` 删除走**二次确认态且显式带会话名**，`y` 才产出
//!   [`PickerAction::Delete`]；**活跃会话按 `d` 直接拒绝 + 可见提示**
//!   （不进确认态——误删活跃会话是本任务 bug 本体）。
//!
//! 三态的纯判定全在 [`PickerState::handle_key`]（无 IO），RPC 副作用只在
//! [`picker_loop`]；[`PickerState::notice`] 承载可见反馈（拒绝 / RPC 失败）。

use std::collections::HashMap;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use web_client::daemon::WebDaemon;
use web_state::types::{ChatMessage, Session, now_epoch_ms};

use crate::termguard::{CrosstermOps, TermGuard};
use crate::theme;
use crate::{TuiError, client_error};

/// 起步/过滤的会话条数上限（picker 是导航，不是数据导出）。
const LIST_LIMIT: u32 = 50;
/// 每个会话取多少条 run 记录判 Active/Aborted 徽章。
const RUN_LIMIT: u32 = 200;
/// 选中后历史回填条数上限（滚动边界以回填结果为准）。
const HISTORY_LIMIT: u32 = 500;
/// 驱动层按键轮询间隔（每次轮询前先画，键来了即刻重画）。
const POLL: Duration = Duration::from_millis(50);

/// 会话行的 run 徽章（route §3 T4：渲染 `runs_for_session` 的
/// Active/Aborted；正常收尾的 run 不挂徽章）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunBadge {
    /// 最新 run 未收尾（在飞）。
    Active,
    /// 最新 run 被显式中止。
    Aborted,
}

/// picker 的一行：会话 + run 徽章。
#[derive(Debug, Clone, PartialEq)]
pub struct PickerItem {
    pub session: Session,
    pub badge: Option<RunBadge>,
}

/// 一次按键的结论（取数/重查由驱动层执行，状态机保持纯）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// 无事发生（含空列表上的 Enter——「无会话」单行不崩）。
    None,
    /// 过滤词变了：驱动层按新词 `search_sessions` 后 [`PickerState::set_items`]
    ///（空词 = 全量列表）。
    Refetch(String),
    /// Enter 选中：携带该行的 session id。
    Select(String),
    /// ESC：返回原会话（驱动层映射成 `Ok(None)`）。
    Cancel,
    /// T16：`n` 新建会话并切过去（驱动层造 id、落库、回填后返回新 id）。
    New,
    /// T16：`r` 提交改名（id + 新标题；空标题按原样发，驱动层不兜底改写）。
    Rename { id: String, title: String },
    /// T16：`d` 二次确认通过（驱动层执行 `delete_session`）。
    Delete { id: String },
}

/// picker 的模态（T16 追加三态；默认 [`PickerMode::List`] 保持 T4 行为）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PickerMode {
    /// 列表态（过滤 + 选择 + 动作派发）。
    #[default]
    List,
    /// 行内改名：编辑缓冲随按键增长，Enter 提交、Esc 放弃。
    Rename { id: String, buffer: String },
    /// 删除二次确认：`name` 是显式回显的会话名（确认态不许只显示一个 id）。
    ConfirmDelete { id: String, name: String },
}

/// picker 纯状态：过滤词 + 结果行 + 高亮游标 + T16 模态/提示（无 IO，
/// 可被测试直接驱动）。
#[derive(Debug, Clone, PartialEq)]
pub struct PickerState {
    filter: String,
    items: Vec<PickerItem>,
    cursor: usize,
    mode: PickerMode,
    /// 当前活跃会话 id（T16 活跃删除防线；空 = 无活跃，如 `--resume` 起步前）。
    active_sid: String,
    /// 可见反馈行（活跃拒绝 / RPC 失败）；`None` = 无提示。
    notice: Option<String>,
}

impl PickerState {
    /// 起步状态（`filter` 即调用方给的初始过滤词，空词 = 全量）。T4 契约：
    /// 无活跃会话、无模态、无提示。
    pub fn new(filter: &str, items: Vec<PickerItem>) -> Self {
        Self {
            filter: filter.to_string(),
            items,
            cursor: 0,
            mode: PickerMode::List,
            active_sid: String::new(),
            notice: None,
        }
    }

    /// 设活跃会话（T16 驱动层调用；`d` 打活跃行时走拒绝分支）。
    pub fn with_active(mut self, sid: &str) -> Self {
        self.active_sid = sid.to_string();
        self
    }

    /// 当前模态（渲染 + 测试断言用）。
    pub fn mode(&self) -> &PickerMode {
        &self.mode
    }

    /// 当前可见提示（`None` = 无提示行）。
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// 驱动层写入可见提示（RPC 失败 / 拒绝），并回到列表态。
    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
        self.mode = PickerMode::List;
    }

    /// 换入重查结果（`search_sessions` 回执）：游标回顶，不越界。
    pub fn set_items(&mut self, items: Vec<PickerItem>) {
        self.items = items;
        self.cursor = 0;
    }

    /// 当前过滤词。
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// 当前结果行。
    pub fn items(&self) -> &[PickerItem] {
        &self.items
    }

    /// 高亮行下标（空列表恒 0，[`Self::selected`] 返回 `None`）。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 高亮行（空列表 = `None`，Enter 因此不选中、不崩）。
    pub fn selected(&self) -> Option<&PickerItem> {
        self.items.get(self.cursor)
    }

    /// 删除成功后把游标夹回列表（row 被摘掉后不越界）。
    pub fn clamp_cursor(&mut self) {
        if !self.items.is_empty() {
            self.cursor = self.cursor.min(self.items.len() - 1);
        } else {
            self.cursor = 0;
        }
    }

    /// 一次按键路由（非 Press/Repeat 忽略；↑↓ 夹紧在列表范围内）。
    ///
    /// T16：先按模态分派——编辑/确认态的按键**不落到过滤词**上（模态盖过
    /// 一切，同 T11 overlay / T13 rewind 确认态的让位口径）；列表态才走
    /// T4 的过滤 + 选择，并把 `n`/`r`/`d` 派成动作。
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return PickerAction::None;
        }
        match self.mode.clone() {
            PickerMode::Rename { id, buffer } => self.rename_key(key, id, buffer),
            PickerMode::ConfirmDelete { id, .. } => self.confirm_delete_key(key, id),
            PickerMode::List => self.list_key(key),
        }
    }

    /// 改名编辑态：Enter 提交、Esc 放弃、其余键进编辑缓冲。
    fn rename_key(&mut self, key: KeyEvent, id: String, buffer: String) -> PickerAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = PickerMode::List;
                PickerAction::None // 放弃 = 零改动
            }
            KeyCode::Enter => {
                self.mode = PickerMode::List;
                PickerAction::Rename { id, title: buffer }
            }
            KeyCode::Backspace => {
                let mut buffer = buffer;
                buffer.pop();
                self.mode = PickerMode::Rename { id, buffer };
                PickerAction::None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut buffer = buffer;
                buffer.push(c);
                self.mode = PickerMode::Rename { id, buffer };
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    /// 删除二次确认态：`y`/Enter 才产出删除意图；`n`/Esc 取消零动作；其余
    /// 键全忽略（盖过列表态的 Enter 选中——模态不许漏泄到别的动作）。
    fn confirm_delete_key(&mut self, key: KeyEvent, id: String) -> PickerAction {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.mode = PickerMode::List;
                PickerAction::Delete { id }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = PickerMode::List;
                PickerAction::None // 取消 = 零改动
            }
            _ => PickerAction::None,
        }
    }

    /// 列表态（T4 过滤/选择 + T16 三个动作键）。
    fn list_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Enter => self
                .selected()
                .map(|item| PickerAction::Select(item.session.id.clone()))
                .unwrap_or(PickerAction::None),
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                PickerAction::None
            }
            KeyCode::Down => {
                if !self.items.is_empty() {
                    self.cursor = (self.cursor + 1).min(self.items.len() - 1);
                }
                PickerAction::None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => match c {
                // T16 动作键占三个单字符；过滤词里要打这三个字用别的键位
                // （过滤优先性让位给动作——与 T9 注册表「命令优先」同向）。
                'n' => PickerAction::New,
                'r' => self.begin_rename(),
                'd' => self.begin_delete(),
                _ => {
                    self.filter.push(c);
                    PickerAction::Refetch(self.filter.clone())
                }
            },
            KeyCode::Backspace => {
                self.filter.pop();
                PickerAction::Refetch(self.filter.clone())
            }
            _ => PickerAction::None,
        }
    }

    /// `r`：选中行进编辑态，缓冲预填当前标题（空标题回退 id，与列表渲染同口径）。
    fn begin_rename(&mut self) -> PickerAction {
        let Some(item) = self.selected() else {
            return PickerAction::None;
        };
        let buffer = if item.session.title.is_empty() {
            item.session.id.clone()
        } else {
            item.session.title.clone()
        };
        self.mode = PickerMode::Rename {
            id: item.session.id.clone(),
            buffer,
        };
        PickerAction::None
    }

    /// `d`：活跃会话**直接拒绝 + 可见提示**（不进确认态——误删活跃会话是
    /// bug 本体）；其余行进二次确认态且显式带会话名。
    fn begin_delete(&mut self) -> PickerAction {
        let Some(item) = self.selected() else {
            return PickerAction::None;
        };
        if item.session.id == self.active_sid {
            let name = display_name(item);
            self.set_notice(format!("不能删除当前会话「{name}」"));
            return PickerAction::None;
        }
        self.mode = PickerMode::ConfirmDelete {
            id: item.session.id.clone(),
            name: display_name(item),
        };
        PickerAction::None
    }
}

/// 会话显示名（空标题回退 id——列表行与确认态回显同口径）。
fn display_name(item: &PickerItem) -> String {
    if item.session.title.is_empty() {
        item.session.id.clone()
    } else {
        item.session.title.clone()
    }
}

/// 画一帧 picker：过滤行 + 结果列表 + **模态/提示行** + 按键提示。
///
/// T16：模态（改名编辑 / 删除确认）与提示（活跃拒绝 / RPC 失败）各占一行，
/// 恒在列表与按键提示之间——列表区高度随之下移一行，窄屏仍由
/// [`PROMPT_COLS`] 与 `Min(1)` 兜底。列表态两行都是空串，不占视觉。
pub fn render(frame: &mut Frame, state: &PickerState) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(frame.area());
    frame.render_widget(Paragraph::new(filter_line(state)), chunks[0]);
    render_list(frame, chunks[1], state);
    frame.render_widget(Paragraph::new(mode_line(state)), chunks[2]);
    frame.render_widget(Paragraph::new(notice_line(state)), chunks[3]);
    frame.render_widget(Paragraph::new(hints_line(state)), chunks[4]);
    // 光标落在 "> " 之后的过滤词末尾（窄屏夹进可视区）。编辑态光标跟着
    // 改名缓冲走，否则输入看着像没进缓冲。
    let (text, offset) = match state.mode() {
        PickerMode::Rename { buffer, .. } => (buffer.as_str(), EDIT_PROMPT.chars().count() as u16),
        _ => (state.filter(), PROMPT_COLS),
    };
    let used = text.chars().count() as u16 + offset;
    let col = used.min(chunks[0].width.saturating_sub(1));
    frame.set_cursor_position(Position::new(chunks[0].x + col, chunks[0].y));
}

/// 过滤行的 "❯ " 前缀列宽（光标定位用）。
const PROMPT_COLS: u16 = 2;
/// 改名编辑行的 "rename ❯ " 前缀列宽（光标定位用，按字符数计）。
const EDIT_PROMPT: &str = "rename ❯ ";

/// 模态行：改名编辑回显缓冲 + 删除确认显式带会话名；列表态空行。
fn mode_line(state: &PickerState) -> Line<'static> {
    match state.mode() {
        PickerMode::List => Line::default(),
        PickerMode::Rename { buffer, .. } => Line::from(vec![
            Span::styled(EDIT_PROMPT.to_string(), theme::brand_bold()),
            Span::styled(buffer.clone(), theme::base()),
        ]),
        PickerMode::ConfirmDelete { name, .. } => Line::from(vec![
            Span::styled(format!("delete 「{name}」? "), theme::danger()),
            Span::styled("y confirm · n/esc cancel", theme::dim()),
        ]),
    }
}

/// 提示行：可见反馈（拒绝 / RPC 失败）；无提示时空行。
fn notice_line(state: &PickerState) -> Line<'static> {
    match state.notice() {
        Some(text) => Line::from(Span::styled(text.to_string(), theme::danger())),
        None => Line::default(),
    }
}

/// 过滤行：提示符 + 过滤词 + 结果计数（dim）。
fn filter_line(state: &PickerState) -> Line<'static> {
    Line::from(vec![
        Span::styled("❯ ".to_string(), theme::brand_bold()),
        Span::styled(state.filter().to_string(), theme::base()),
        Span::styled(format!("  ({})", state.items().len()), theme::dim()),
    ])
}

/// 结果列表：空 → 「无会话」单行；非空 → 按高亮游标取可视窗口。
fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, state: &PickerState) {
    if state.items().is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("无会话", theme::dim()))),
            area,
        );
        return;
    }
    let height = area.height.max(1) as usize;
    // 视窗贴住高亮行（长列表只显示能放下的尾段，不越界）。
    let scroll = state.cursor().saturating_sub(height.saturating_sub(1));
    let rows: Vec<Line> = state
        .items()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, item)| row_line(i == state.cursor(), item))
        .collect();
    frame.render_widget(Paragraph::new(rows), area);
}

/// 一行会话：游标前缀 + Active/Aborted 徽章 + 标题（空标题回退 id）。
fn row_line(selected: bool, item: &PickerItem) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if selected { "❯ " } else { "  " },
        if selected {
            theme::brand_bold()
        } else {
            theme::dim()
        },
    )];
    if let Some(badge) = item.badge {
        let (label, style) = match badge {
            RunBadge::Active => ("[Active] ", theme::brand_bold()),
            RunBadge::Aborted => ("[Aborted] ", theme::danger()),
        };
        spans.push(Span::styled(label.to_string(), style));
    }
    let title = if item.session.title.is_empty() {
        item.session.id.clone()
    } else {
        item.session.title.clone()
    };
    spans.push(Span::styled(
        title,
        if selected {
            theme::brand()
        } else {
            theme::base()
        },
    ));
    Line::from(spans)
}

/// 按键提示行（恒在最底一行）。T16 追加三个动作键的提示。
fn hints_line(state: &PickerState) -> Line<'static> {
    let text = match state.mode() {
        PickerMode::List => "enter select · esc cancel · ↑↓ move · n new · r rename · d delete",
        PickerMode::Rename { .. } => "enter save · esc cancel",
        PickerMode::ConfirmDelete { .. } => "y confirm · n/esc cancel",
    };
    Line::from(Span::styled(text.to_string(), theme::dim()))
}

/// 选中后的历史回填数据源（route §3 T4：`load_messages`）。
pub fn load_history(client: &WebDaemon, sid: &str) -> Result<Vec<ChatMessage>, TuiError> {
    client
        .load_messages(sid, HISTORY_LIMIT)
        .map_err(client_error)
}

/// 会话导航 + 管理 picker（route §3 T4 + T16）。
///
/// 行为契约见模块文档。`active_sid` = 当前活跃会话 id（T16 活跃删除防线；
/// 传空串 = 无活跃，`d` 一律进确认态）。终端进出：raw 已开（enhanced 外壳
/// 内）= 调用方管终端，本函数只接管绘制、退出不碰终端状态；raw 未开（独立
/// 调用）经 [`TermGuard`] 进 alternate screen + raw，退出/panic 都还原。
pub fn run_picker(
    client: &WebDaemon,
    active_sid: &str,
    filter: &str,
) -> Result<Option<String>, TuiError> {
    let mut badges: HashMap<String, Option<RunBadge>> = HashMap::new();
    let mut state =
        PickerState::new(filter, fetch_items(client, filter, &mut badges)?).with_active(active_sid);
    // 嵌套判定：raw 已开 = 调用方（run_enhanced 的 TermGuard）管终端。
    let nested = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    let mut guard = TermGuard::new(CrosstermOps);
    if !nested {
        // 独立调用：panic 先还原再打印（同 run_enhanced 的底线）。
        crate::termguard::install_panic_hook();
        if let Err(e) = guard.enter() {
            let _ = guard.leave();
            return Err(TuiError::Io(e));
        }
    }
    let picked = picker_loop(client, &mut state, &mut badges);
    let leave = guard.leave();
    match (picked, leave) {
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(TuiError::Io(err)),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// 驱动循环：画 → 读键 → [`PickerAction`] 分派（重查只发生在
/// [`PickerAction::Refetch`] 与 T16 三个写动作之后，每次按键一查、空词回落
/// 全量）。
///
/// T16 的三个写动作**失败不致命**：写进 `state.notice` 继续留在 picker
/// （route §3 T16「失败态可见、不静默、不假刷新」）——只有 IO 错误才向上抛。
fn picker_loop(
    client: &WebDaemon,
    state: &mut PickerState,
    badges: &mut HashMap<String, Option<RunBadge>>,
) -> Result<Option<String>, TuiError> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    loop {
        terminal.draw(|frame| render(frame, state))?;
        if !event::poll(POLL)? {
            continue;
        }
        // resize：下一轮 draw 的 autoresize 自动重排；paste/focus 不改状态。
        match event::read()? {
            Event::Key(key) => match state.handle_key(key) {
                PickerAction::None => {}
                PickerAction::Cancel => return Ok(None),
                PickerAction::Select(id) => return Ok(Some(id)),
                PickerAction::Refetch(query) => {
                    state.set_items(fetch_items(client, &query, badges)?);
                }
                // T16 新建：空列表也能按（空列表正是新建的主入口场景）。
                // 造 id 口径同 [`crate::resolve_session`] 的 `s-{epoch}`，
                // create 幂等，标题留占位由首条消息派生。
                PickerAction::New => {
                    let ms = now_epoch_ms();
                    let sid = format!("s-{ms}");
                    match client.create_session(&sid, &format!("会话 {ms}")) {
                        Ok(()) => return Ok(Some(sid)),
                        Err(err) => state.set_notice(format!("新建会话失败: {err}")),
                    }
                }
                PickerAction::Rename { id, title } => {
                    let result = client.update_session_title(&id, &title);
                    badges.remove(&id); // 标题变了，重查行要重新取徽章无关项
                    match result {
                        Ok(()) => {
                            let query = state.filter().to_string();
                            state.set_items(fetch_items(client, &query, badges)?);
                        }
                        Err(err) => state.set_notice(format!("改名失败: {err}")),
                    }
                }
                // T16 删除：活跃保护已在纯状态机里拦掉（`d` 就没进确认态），
                // 到这里的 id 必非活跃。成功后重查并夹回游标（行被摘掉）。
                PickerAction::Delete { id } => match client.delete_session(&id) {
                    Ok(()) => {
                        badges.remove(&id);
                        let query = state.filter().to_string();
                        state.set_items(fetch_items(client, &query, badges)?);
                        state.clamp_cursor();
                    }
                    Err(err) => state.set_notice(format!("删除失败: {err}")),
                },
            },
            _ => {}
        }
    }
}

/// 按词取会话行 + 每行 run 徽章。
///
/// 空词 = `list_sessions` 全量起步；非空 = `search_sessions` 过滤。
/// 徽章按会话缓存在驱动层（`runs_for_session` 是整本 ledger 的客户端过滤，
/// 每次按键重查全部会话会退化成 N 次全文件读）。
fn fetch_items(
    client: &WebDaemon,
    filter: &str,
    badges: &mut HashMap<String, Option<RunBadge>>,
) -> Result<Vec<PickerItem>, TuiError> {
    let sessions = if filter.trim().is_empty() {
        client.list_sessions(LIST_LIMIT).map_err(client_error)?
    } else {
        client
            .search_sessions(filter, LIST_LIMIT)
            .map_err(client_error)?
    };
    Ok(sessions
        .into_iter()
        .map(|session| {
            let badge = match badges.get(&session.id) {
                Some(cached) => *cached,
                None => {
                    let badge = run_badge(client, &session.id);
                    badges.insert(session.id.clone(), badge);
                    badge
                }
            };
            PickerItem { session, badge }
        })
        .collect())
}

/// 最新 run 的徽章：半开（`finished_at_ms` 为空）→ Active；`aborted` →
/// Aborted；正常收尾 / 无 run / RPC 失败 → 无徽章（徽章是装饰，不许把
/// picker 拖死）。
fn run_badge(client: &WebDaemon, sid: &str) -> Option<RunBadge> {
    let runs = client.runs_for_session(sid, RUN_LIMIT).ok()?;
    let latest = runs.iter().max_by_key(|run| run.seq)?;
    if latest.finished_at_ms.is_none() {
        return Some(RunBadge::Active);
    }
    if latest.status.as_deref() == Some("aborted") {
        return Some(RunBadge::Aborted);
    }
    None
}
