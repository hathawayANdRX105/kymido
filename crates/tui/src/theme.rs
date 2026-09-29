//! theme.rs — D11：enhanced 外壳的颜色/样式唯一入口（token 名对齐 web
//! 的 brand / dim / danger）。全部走 ratatui 命名色：不写 hex 色值、不写
//! ESC 转义字面量（`tests/theme_lint.rs` 扫全 `crates/tui/src` 钉住），
//! 配色决定权留给终端调色板，浅色/深色终端都可读（decisions D11）。
//!
//! `crates/tui/src` 内任何样式都必须从本模块出发；ui/* 只许 import 本模块
//! 的语义函数，不许把 `Color` 构造泄漏到调用点。
//!
//! T17（route §3 T17）：语义函数**签名不变**，取值改由
//! [`Scheme`]（当前活跃方案）经 [`color`] 单表供给——组件调用点零改动是
//! 硬指标。方案存在进程级槽 [`ACTIVE`]：面板切换即改，重绘按新值取色。
//! 单表语义照 dh-rs `tui/theme.rs` 的 palette 对位（组件持语义、表供值、
//! 组件永不持字面）。首版只两档 scheme，后续扩档只加表项、接线不动。

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::{Color, Modifier, Style};

/// 配色方案（亮 / 暗）。T17 首版两档——扩档只加枚举变体 + [`Scheme::ALL`]
/// 表项与 [`Scheme::color`] 一行，面板与配置读取都按表迭代，零接线改动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    /// 暗背景终端（默认，serde default 同值）。
    #[default]
    Dark,
    /// 亮背景终端。
    Light,
}

impl Scheme {
    /// 候选表：面板按此渲染，配置按 `name()` 往返——**扩档只动这里与
    /// [`Self::color`]**。
    pub const ALL: [Scheme; 2] = [Scheme::Dark, Scheme::Light];

    /// 稳定名（`/theme` 面板文案 + 配置值同源）。
    pub fn name(&self) -> &'static str {
        match self {
            Scheme::Dark => "dark",
            Scheme::Light => "light",
        }
    }

    /// 按名解析；未知 / 大小写不符 → `None`（调用方回落默认，不 panic）。
    pub fn from_name(name: &str) -> Option<Scheme> {
        Scheme::ALL.into_iter().find(|s| s.name() == name)
    }

    /// 调色板单表：`(方案, 语义) → 命名色`。组件永远不持色值，只调
    /// [`brand`] / [`dim`] / [`danger`]（D11 的收敛点由
    /// `tests/theme_palette.rs` 钉住）。
    fn color(self, role: Role) -> Color {
        match (self, role) {
            // brand：暗背景要亮色才够对比，亮背景换中蓝免得刺眼。
            (Scheme::Dark, Role::Brand) => Color::LightBlue,
            (Scheme::Light, Role::Brand) => Color::Blue,
            // dim：暗底靠 DarkGray，亮底 DarkGray 几乎看不见 → 改中灰。
            (Scheme::Dark, Role::Dim) => Color::DarkGray,
            (Scheme::Light, Role::Dim) => Color::Gray,
            // danger：亮底上 LightRed 太浅，换深红保住可读性。
            (Scheme::Dark, Role::Danger) => Color::LightRed,
            (Scheme::Light, Role::Danger) => Color::Red,
            // border：面板与卡的轮廓。暗底用 DarkGray 免得抢正文；亮底
            // DarkGray 几乎看不见（同 dim 档的既有事故），改中灰。
            (Scheme::Dark, Role::Border) => Color::DarkGray,
            (Scheme::Light, Role::Border) => Color::Gray,
            // success：工具成功的终局绿。亮底上 LightGreen 太浅，换深绿。
            (Scheme::Dark, Role::Success) => Color::LightGreen,
            (Scheme::Light, Role::Success) => Color::Green,
            // warn：折叠提示、需要留意但不是错误。暗底 Yellow 够亮；亮底
            // Yellow 在白底上基本消失 → 深黄。
            (Scheme::Dark, Role::Warn) => Color::LightYellow,
            (Scheme::Light, Role::Warn) => Color::Yellow,
            // heading：标题。暗底 LightCyan 够亮且与 brand 的 LightBlue、
            // warn 的 LightYellow 都分得开；亮底 Cyan 反差足够。
            (Scheme::Dark, Role::Heading) => Color::LightCyan,
            (Scheme::Light, Role::Heading) => Color::Cyan,
            // code_fg：代码与行内 code 的前景。DarkGray/Gray 在亮底不可读
            // 是本文件记录过的既有事故，两档都取中灰——代码靠 `▌` 左条与
            // 缩进识别，前景不需要抢眼，只需要在两种终端上都读得出。
            (Scheme::Dark, Role::CodeFg) => Color::Gray,
            (Scheme::Light, Role::CodeFg) => Color::Gray,
            // diff_add / diff_del：新增与删除。绿/红的自然语义，与 success /
            // danger 同色是刻意的——跨角色的色彩复用在本文件有先例（border
            // 与 dim 同色），语义对齐比另造色重要。
            (Scheme::Dark, Role::DiffAdd) => Color::LightGreen,
            (Scheme::Light, Role::DiffAdd) => Color::Green,
            (Scheme::Dark, Role::DiffDel) => Color::LightRed,
            (Scheme::Light, Role::DiffDel) => Color::Red,
        }
    }
}

/// 语义角色（`theme.rs` 私有——组件只见 [`brand`] / [`dim`] / [`danger`]）。
#[derive(Debug, Clone, Copy)]
enum Role {
    Brand,
    Dim,
    Danger,
    Border,
    Success,
    Warn,
    Heading,
    CodeFg,
    DiffAdd,
    DiffDel,
}

/// 面板与卡的轮廓色。比 dim 更冷一档，浅色终端上仍与正文分得开。
pub fn border() -> Style {
    Style::default().fg(color(Role::Border))
}

/// 成功终局：工具跑完、任务 Done。刻意与 danger 成对，失败面一眼可分。
pub fn success() -> Style {
    Style::default().fg(color(Role::Success))
}

/// 需要留意但不是错误：折叠提示、行数截断、被跳过的工具。
pub fn warn() -> Style {
    Style::default().fg(color(Role::Warn))
}

/// markdown 标题（T26）：与正文的层级区分靠加粗 + `#` 前缀 + 本色。
pub fn heading() -> Style {
    Style::default().fg(color(Role::Heading))
}

/// 代码前景（T26/T27）：围栏代码块与行内 code。刻意不抢眼——代码靠
/// `▌` 左条与缩进识别，前景只需要在两种终端上都读得出。
pub fn code_fg() -> Style {
    Style::default().fg(color(Role::CodeFg))
}

/// diff 新增行（T27）：与 [`success`] 同色是刻意的语义对齐。
pub fn diff_add() -> Style {
    Style::default().fg(color(Role::DiffAdd))
}

/// diff 删除行（T27）：与 [`danger`] 同色是刻意的语义对齐。
pub fn diff_del() -> Style {
    Style::default().fg(color(Role::DiffDel))
}

/// 进程级活跃方案槽（`AtomicU8`：值 = [`Scheme::ALL`] 下标 + 1，0 = 未
/// 初始化 → 读作 [`Scheme::default`]）。面板切换写它，重绘读它；`App`
/// 侧无需持状态，故 linear / inline 渲染路径天然零改动。
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// 当前活跃方案（未设置过 = [`Scheme::Dark`]）。
pub fn active() -> Scheme {
    match ACTIVE.load(Ordering::Relaxed) {
        1 => Scheme::Light,
        2 => Scheme::Dark,
        _ => Scheme::Dark,
    }
}

/// 切到指定方案（面板选中即调；重绘由调用方触发——本模块不碰终端）。
pub fn set_active(scheme: Scheme) {
    let slot = match scheme {
        Scheme::Dark => 2,
        Scheme::Light => 1,
    };
    ACTIVE.store(slot, Ordering::Relaxed);
}

/// 按名切换；未知名 → 回落 [`Scheme::Dark`] 并返回实际生效的方案
/// （配置读不到 / 非法值不许 panic，也不许静默失败——调用方拿到真值）。
pub fn set_by_name(name: &str) -> Scheme {
    let scheme = Scheme::from_name(name).unwrap_or_default();
    set_active(scheme);
    scheme
}

/// 单表取色：语义 + 当前活跃方案 → 命名色。
fn color(role: Role) -> Color {
    active().color(role)
}

/// 正文默认：不着色，跟随终端前景色。
pub fn base() -> Style {
    Style::default()
}

/// brand（web `--color-brand` 同族）：强调、用户侧、运行中。
pub fn brand() -> Style {
    Style::default().fg(color(Role::Brand))
}

/// brand 加粗：状态点、composer 提示符、transcript 用户前缀。
pub fn brand_bold() -> Style {
    brand().add_modifier(Modifier::BOLD)
}

/// dim（web `--color-dim` 同族）：辅助信息、按键提示、空闲态、工具行。
pub fn dim() -> Style {
    Style::default().fg(color(Role::Dim))
}

/// danger（web `--color-danger` 同族）：错误、中断、退出确认。
pub fn danger() -> Style {
    Style::default().fg(color(Role::Danger))
}

/// T12：焦点消息行标记——焦点消息的首条可见行整体加下划线（route §3 T12
/// 注记④「焦点消息行最小标记」）。不引入新颜色，标记靠字形属性，浅色/深色
/// 终端同样可读（D11 口径）。
pub fn focus() -> Style {
    base().add_modifier(Modifier::UNDERLINED)
}

/// 启动读定的起始方案：`Config::load()` 的 `[tui] theme`——读不到 / 解析
/// 失败 / 值不认识都落回 [`Scheme::Dark`]（不 panic；route §1 已知坑：启动
/// 读定，不热载）。三个入口（enhanced / inline / linear）共用这一个读取口。
pub fn active_from_config() -> Scheme {
    match config::Config::load() {
        Ok(cfg) => set_by_name(&cfg.tui_theme),
        Err(_) => {
            set_active(Scheme::Dark);
            Scheme::Dark
        }
    }
}
