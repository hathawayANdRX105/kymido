//! 全仓统一**语义样式常量**：把 rsx 里反复手抄的 Tailwind/dsh class 串收敛到单一来源。
//!
//! 这些常量是视觉 token，不是逻辑（对照 ferrite `ui-components/styles.rs`）。
//! 页面/组件只引用常量，不在 `rsx!` 里写重复长串；改一处即可全仓生效。
//!
//! 本轮是**纯收敛**：常量值 = 现状 dsh 串逐字保留（`text-label-*`/`bg-layer-*`/
//! `border-b*`/`shadow-lv*`/`bg-chip-*`），**不改任何色值**。把 dsh 命名色板换成
//! ui-kit 的 shadcn 语义 token（`bg-card`/`text-muted-foreground`/`border-border`…）
//! 是独立的收敛 PR，本文件那时只改常量右值、调用点零改动（这正是收敛成常量的意义）。

// ===========================================================================
// ① 表面（背景）——按语义引用，不按色板名
// ===========================================================================

/// 页面/根底：`bg-base`（omonic 页面底色，dsh `--color-base`）。
pub const S_PAGE: &str = "bg-base";

/// 面板/卡底：`bg-layer-1`（最常用的内容卡面）。
pub const S_PANEL: &str = "bg-layer-1";

/// 凸起面：`bg-layer-2`（卡内嵌套块、任务看板面）。
pub const S_RAISED: &str = "bg-layer-2";

/// 最深面：`bg-layer-3`（进度条轨道、选中底）。
pub const S_DEEPEST: &str = "bg-layer-3";

/// 侧栏底：`bg-sidebar`。
pub const S_SIDEBAR: &str = "bg-sidebar";

/// 输入卡底：`bg-input-bg`（composer 胶囊）。
pub const S_INPUT: &str = "bg-input-bg";

/// 弹层/菜单底：`bg-menu`。
pub const S_MENU: &str = "bg-menu";

/// 代码块底：`bg-codeblock`。
pub const S_CODEBLOCK: &str = "bg-codeblock";

/// 选中条：`bg-selector`（问题卡选项钮、附件钮）。
pub const S_SELECTOR: &str = "bg-selector";

/// 用户气泡底：`bg-bubble`。
pub const S_BUBBLE: &str = "bg-bubble";

/// 交互 hover 底：`bg-ihover`（白 8%）。
pub const S_HOVER: &str = "bg-ihover";

/// 交互 active 底：`bg-iactive`（白 14%）。
pub const S_ACTIVE: &str = "bg-iactive";

/// 弱化圆点：`bg-dim`（Idle 状态点）。
pub const S_DIM: &str = "bg-dim";

/// 强调面：`bg-brand`（本仓当前 dsh 蓝；收敛 PR 会换成 kit primary 语义）。
pub const S_BRAND: &str = "bg-brand";

// ===========================================================================
// ② 描边
// ===========================================================================

/// 一级描边 `border-b1`（最常用分隔线/卡边）。
pub const B_1: &str = "border-b1";

/// 二级描边 `border-b2`（悬停强调的边）。
pub const B_2: &str = "border-b2";

/// 三级描边 `border-b3`（focus-within 提亮的边）。
pub const B_3: &str = "border-b3";

/// 反相描边 `border-binv`（弹层/面板的更弱边）。
pub const B_INV: &str = "border-binv";

/// 强调描边 `border-brand`。
pub const B_BRAND: &str = "border-brand";

// ===========================================================================
// ③ 文字色（按语义，不按色号）
// ===========================================================================

/// 主文字 `text-label`（最亮，标题/正文主色）。
pub const C_TEXT: &str = "text-label";

/// 次级文字 `text-label-2`。
pub const C_TEXT2: &str = "text-label-2";

/// 弱化文字 `text-label-3`（说明/辅助）。
pub const C_MUTED: &str = "text-label-3";

/// 最弱 `text-caption`（脚注/小注）。
pub const C_CAPTION: &str = "text-caption";

/// 强调文字 `text-brand`。
pub const C_BRAND: &str = "text-brand";

/// 亮强调 `text-brand-300`（工具 kind / 计划评审标）。
pub const C_BRAND_L: &str = "text-brand-300";

/// 危险/错误 `text-danger`。
pub const C_DANGER: &str = "text-danger";

/// 成功 `text-success-2`。
pub const C_SUCCESS: &str = "text-success-2";

/// 警告 `text-warn-2`。
pub const C_WARN: &str = "text-warn-2";

/// 成功 chip 底 `bg-chip-success`。
pub const S_CHIP_SUCCESS: &str = "bg-chip-success";

/// 危险 chip 底 `bg-chip-danger`。
pub const S_CHIP_DANGER: &str = "bg-chip-danger";

/// 警告 chip 底 `bg-chip-warn`。
pub const S_CHIP_WARN: &str = "bg-chip-warn";

/// 强调 chip 底 `bg-chip-brand`。
pub const S_CHIP_BRAND: &str = "bg-chip-brand";

// ===========================================================================
// ④ 阴影
// ===========================================================================

/// 中卡投影 `shadow-lv2`（composer/问题卡/看板）。
pub const SHADOW_CARD: &str = "shadow-lv2";

/// 大浮层投影 `shadow-lv3`（悬停信息面板）。
pub const SHADOW_POP: &str = "shadow-lv3";

// ===========================================================================
// ⑤ 字号行高（text scale，常用组合）
// ===========================================================================

/// 14px/20px 正文行（组件内最常用的尺寸）。
pub const TYPE_BODY: &str = "text-[14px] leading-5";

/// 14px/20px medium 标题。
pub const TYPE_TITLE: &str = "text-[14px] leading-5 font-medium";

/// 13px/20px 次级行。
pub const TYPE_DESC2: &str = "text-[13px] leading-5";

/// 12px/20px 弱化行（时间戳/说明）。
pub const TYPE_SMALL: &str = "text-[12px] leading-5";

/// 12px/16px 小注行（caption 高密度）。
pub const TYPE_CAPTION: &str = "text-[12px] leading-4";

/// 11px/16px 微注（mono 计数/编号）。
pub const TYPE_TINY: &str = "text-[11px] leading-4";

/// 10px/16px 极微（chip/大写标签）。
pub const TYPE_MICRO: &str = "text-[10px] leading-4";

// ===========================================================================
// ⑥ 组合件外壳（>=2 处同构的语义壳，B2.5 收敛点）
// ===========================================================================

/// 内容卡壳：`rounded border + 面板底`（设置卡/任务卡/统计卡共用）。
pub const CARD: &str = "rounded-2xl border border-b1 bg-layer-1";

/// hover 可点卡壳（任务卡/列表行）：未选中态。
pub const CARD_ROW: &str =
    "rounded-[10px] border border-b1 bg-layer-1 px-3 py-2.5 cursor-pointer transition-colors";

/// hover 可点卡壳：选中态（brand 边）。
pub const CARD_ROW_ON: &str =
    "rounded-[10px] border border-brand bg-layer-1 px-3 py-2.5 cursor-pointer transition-colors";

/// 侧栏激活导航行。
pub const NAV_ON: &str = "h-10 px-3 rounded-xl flex items-center gap-2.5 text-[14px] leading-[22px] bg-ihover text-label cursor-pointer transition-colors border-none w-full";

/// 侧栏未激活导航行。
pub const NAV_OFF: &str = "h-10 px-3 rounded-xl flex items-center gap-2.5 text-[14px] leading-[22px] text-label-2 hover:bg-ihover hover:text-label cursor-pointer transition-colors border-none w-full bg-transparent";

/// 表单输入框（设置页 text input 外壳）。
pub const INPUT_FIELD: &str = "w-full h-9 rounded-[10px] bg-layer-2 border border-b2 px-3 text-[14px] leading-[22px] text-label outline-none transition-colors focus:border-brand placeholder:text-caption";

/// 提示条成功壳（连接成功 / 保存成功）。
pub const BAR_OK: &str =
    "px-4 py-2.5 rounded-[10px] text-[13px] leading-5 bg-chip-success text-success-2";

/// 提示条危险壳（连接失败 / 保存失败）。
pub const BAR_ERR: &str =
    "px-4 py-2.5 rounded-[10px] text-[13px] leading-5 bg-chip-danger text-danger";
