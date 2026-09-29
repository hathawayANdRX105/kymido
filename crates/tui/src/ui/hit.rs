//! ui/hit — 渲染物化侧登记的命中表（route §3 批 A：点击路由）。
//!
//! 唯一数据通路是**物化侧**：`transcript::window` 在把每个工具卡头 / 思考块头 /
//! Hidden stub 画到帧上时，顺手套一个 [`HitRegion`]{x,y,w,h,action} 推进本表；
//! `ui::draw` 末尾把回到底部 overlay 推进表。点击坐标 → 命中 → [`HitAction`]
//! 反向派发，命中行号天然 = 物化画出的行号（不从 count 侧推——后者没有屏幕
//! 坐标）。后登记的优先，overlay 盖住下方的 transcript 行。
//!
//! 边界：只在 enhanced 用（`App` 的 `hit: RefCell<HitMap>` 只被 `event_loop`
//! 的 `Down(Left)` 派发与 `ui::draw` 的登记用）。`inline`/`linear` 不调用
//! [`Self::register`]，鼠标也不接事件（route §8：点击是 enhanced 专属）。全表
//! 数据走 `theme` 样式，不持有 `Color`。

/// 点击落到命中区要派发的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitAction {
    /// 回到底部（脱钩态 overlay 按钮）：等价 `scroll.to_end()`。
    ScrollToBottom,
    /// 工具卡头：循环三态折叠。
    ToggleToolCard {
        /// `app.messages()` 下标。
        msg_idx: usize,
        /// `msg.parts` 下标。
        part_idx: usize,
    },
    /// 思考块头：循环三态折叠（reasoning 是消息级字段，part_idx 用哨兵位）。
    ToggleThinking {
        /// `app.messages()` 下标。
        msg_idx: usize,
    },
}

/// 一条命中区：屏幕上 `[x, x+w) × [y, y+h)` 这块矩形点下去 → `action`。
#[derive(Debug, Clone, Copy)]
pub struct HitRegion {
    /// 矩形左上角列（帧绝对坐标）。
    pub x: u16,
    /// 矩形左上角行（帧绝对坐标）。
    pub y: u16,
    /// 宽（格）。0 = 不可命中。
    pub w: u16,
    /// 高（格）。0 = 不可命中。
    pub h: u16,
    /// 命中后派发的动作。
    pub action: HitAction,
}

/// 一帧的命中表。每帧 `clear()` 后由渲染路径重建——几何随断行/滚动每帧变化，
/// 不跨帧缓存（StaleMap 陷阱：resize/折叠会让旧坐标指向错误目标）。
#[derive(Debug, Default)]
pub struct HitMap {
    regions: Vec<HitRegion>,
}

impl HitMap {
    /// 清表（每帧渲染前调，登记与绘制同帧同序）。
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    /// 登记一条命中区。`w == 0 || h == 0` 直接丢弃（画不出来就不该点）。
    pub fn push(&mut self, x: u16, y: u16, w: u16, h: u16, action: HitAction) {
        if w == 0 || h == 0 {
            return;
        }
        self.regions.push(HitRegion { x, y, w, h, action });
    }

    /// 点 `(col,row)` 命中的动作；`None` = 落在空白/未登记区。后登记的优先
    /// ——overlay（回到底部按钮，帧尾登记）压在 transcript 行上时赢。
    pub fn hit(&self, col: u16, row: u16) -> Option<HitAction> {
        self.regions
            .iter()
            .rev()
            .find(|r| col >= r.x && col < r.x + r.w && row >= r.y && row < r.y + r.h)
            .map(|r| r.action)
    }
}
