//! 页面导航 / 设置分区类型：views ↔ components 共享的低层落点
//! （web_layering：views → components/layouts/utils 单向，共享类型进低层）。

use crate::shared as sh;

/// 导航视图（中栏页面切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// 会话页（chat 对话）
    Chat,
    /// 统计页（时间范围走 stats_range 信号）
    Stats,
    /// 设置页（分区走 settings_section 信号；弹窗形态已退役）
    Settings,
    /// 归档页（软删会话列表；恢复 / 彻底删除）
    Archive,
}

/// 设置分区：侧栏「设置」二级菜单选项 ↔ 中栏页面一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Models,
    Mcp,
    About,
}

impl SettingsSection {
    /// 分区展示名（页面头 / 侧栏二级菜单行文案同源）。
    pub const fn label(self) -> &'static str {
        match self {
            Self::Models => sh::LBL_MODEL_CHANNEL,
            Self::Mcp => sh::LBL_MCP_SERVERS,
            Self::About => sh::LBL_ABOUT,
        }
    }

    /// 侧栏二级菜单选项 id ↔ 分区；未识别 id 返回 `None`（调用方自行忽略）。
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "models" => Some(Self::Models),
            "mcp" => Some(Self::Mcp),
            "about" => Some(Self::About),
            _ => None,
        }
    }
}
