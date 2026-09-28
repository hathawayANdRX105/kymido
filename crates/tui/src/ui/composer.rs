//! ui/composer — dock 的输入行（route §3 四件套之一：非空 Enter 提交、
//! ↑/↓ 走本地历史；编辑语义全在 `app::App`，这里只负责显示与光标）。

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// composer 提示符：与 transcript 用户前缀同一个 `❯`（T26 验收反馈：输入
/// 侧符号只保留这一个，`>` 弃用）。
const PROMPT: &str = "❯ ";

/// 渲染输入行并把光标钉在插入点。
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let prompt_cols = UnicodeWidthStr::width(PROMPT);
    // append-only 编辑：输入超宽时横向滚动，只留末尾能放下的部分
    //（视窗永远贴着插入点，长输入不会把行撑出 dock）。
    let visible = (area.width as usize).saturating_sub(prompt_cols).max(1);
    let (window, window_cols) = tail_cells(app.input(), visible);
    let line = Line::from(vec![
        Span::styled(PROMPT, theme::brand_bold()),
        Span::styled(window, theme::base()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    // 光标钉在可视窗口末尾，位置按**显示格宽**算（夹进可视区，窄终端下
    // 不许写到 dock 之外）。按字符数算的话，输了一行中文时光标会落在
    // 半个窗口处——插入点其实在行尾。
    let col = (prompt_cols as u16 + window_cols as u16).min(area.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(area.x + col, area.y));
}

/// 取 `s` 末尾至多 `cells` 个显示格的那一段，并返回它在终端里实际占的
/// 格数（T23）。
///
/// 按 `chars().rev().take(visible)` 取尾部是错的：CJK 与 emoji 一个字符
/// 占 2 格，取 `visible` 个字符能撑出 `2 × visible` 格，把 dock 顶破、
/// 让光标列号与实际插入点差出一大截。组合符宽度 0，跟着基字符一起进，
/// 不单独占格。
fn tail_cells(s: &str, cells: usize) -> (String, usize) {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in s.chars().rev() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0) as usize;
        if used + w > cells {
            break;
        }
        used += w;
        kept.push(c);
    }
    kept.reverse();
    (kept.into_iter().collect(), used)
}
