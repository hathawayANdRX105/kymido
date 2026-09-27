//! ui/theme_panel — T17 `/theme` 方案选择面板（候选 = `theme::Scheme::ALL`）。
//!
//! 只渲染：开合 / 游标全在 [`App`]，这里读 [`App::theme_panel_open`] /
//! [`App::theme_scheme_selected`]；行数与斜杠面板**共用** [`App::panel_rows`]
//! （两者互斥，`ui::areas` 按那一个数从 transcript 让行，渲染与让行不漂移）。
//! 样式全部走 `crate::theme`（D11，无 ESC 字节）——本面板自己也被主题染色，
//! 切方案后下一帧即可见效果（T17「即时重绘」）。

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::{self, Scheme};

/// 画方案候选到 `area`（0 行 = 面板关闭，直接返回；超出则截尾）。
pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if area.height == 0 || !app.theme_panel_open() {
        return;
    }
    let selected = app.theme_scheme_selected();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(Scheme::ALL.len());
    for scheme in Scheme::ALL {
        let highlighted = selected == Some(scheme);
        let mut spans = vec![
            Span::styled(
                if highlighted { "> " } else { "  " }.to_string(),
                theme::brand_bold(),
            ),
            Span::styled(
                scheme.name().to_string(),
                if highlighted {
                    theme::brand_bold()
                } else {
                    theme::base()
                },
            ),
        ];
        if scheme == theme::active() {
            spans.push(Span::styled("  (active)", theme::dim()));
        }
        lines.push(Line::from(spans));
    }
    lines.truncate(area.height as usize);
    frame.render_widget(Paragraph::new(lines), area);
}
