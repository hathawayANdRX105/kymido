//! web-ui 共享**文案常量**：rsx 元素体内中文清零（web-spec H3）。
//!
//! 按 ferrite `admin-page-users/shared.rs` 前缀分层：值即原字面量，一个字符不改
//! （H1 零渲染变化）。边界：不放网络调用、不放组件。
//!
//! | 前缀 | 用途 |
//! |---|---|
//! | `BTN_` | 按钮/触发器文案 |
//! | `LBL_` | 标签/表头/字段名/区段名 |
//! | `SEC_` | 区段标题 |
//! | `MSG_` | 提示/错误/空态 |
//! | `OPT_` | 下拉/选项文案 |
//! | `STATUS_` | 状态徽标 |
//! | `TTL_` | 弹窗/页面标题 |

// ---- BTN_ : 按钮 / 触发器 ----
pub const BTN_SEND: &str = "发送";
pub const BTN_STOP: &str = "停止";
pub const BTN_CLOSE: &str = "关闭";
pub const BTN_REMOVE: &str = "移除";
pub const BTN_DELETE_SESSION: &str = "删除会话";
pub const BTN_DELETE: &str = "删除";
pub const BTN_ADD_IMAGE: &str = "添加图片附件";
pub const BTN_REMOVE_SPACE: &str = "移除项目";
pub const BTN_CREATE_PROJECT: &str = "创建项目（选择目录）";
pub const BTN_NEW_SESSION: &str = "新会话";
pub const BTN_NEW_CHAT_IN_SPACE: &str = "新建会话";
pub const BTN_SAVE_CONFIG: &str = "保存配置";
pub const BTN_SAVE_MCP: &str = "保存 MCP 配置";
pub const BTN_EXPAND_SIDEBAR: &str = "展开侧边栏";
pub const BTN_COLLAPSE_SIDEBAR: &str = "收起侧边栏";
pub const BTN_TASK_PANEL: &str = "任务看板";
pub const BTN_COPY: &str = "复制";
pub const BTN_ARCHIVE_SESSION: &str = "归档会话";
pub const BTN_RESTORE_SESSION: &str = "恢复会话";
pub const BTN_PURGE_SESSION: &str = "彻底删除";

// ---- LBL_ : 标签 / 表头 / 区段名 ----
pub const LBL_SESSIONS: &str = "会话";
pub const LBL_STATS: &str = "数据统计";
pub const LBL_SETTINGS: &str = "设置";
pub const LBL_ARCHIVE: &str = "归档";
pub const LBL_ABOUT: &str = "关于";
pub const LBL_MODEL_CHANNEL: &str = "模型与渠道";
pub const LBL_MCP_SERVERS: &str = "MCP 服务器";
pub const LBL_THINKING_STRENGTH: &str = "思考强度";
pub const LBL_THINKING: &str = "思考";
pub const LBL_PICK_MODEL: &str = "选择模型";
pub const LBL_PICK_SESSION: &str = "选择会话快速切换";
pub const LBL_PLAN_REVIEW: &str = "计划评审";
/// Work Process 块头行：进行中动词形 / 结束后名词形（ainotation 波4 #1）
pub const LBL_WORK_PROGRESSING: &str = "Progressing";
pub const LBL_WORK_PROGRESS: &str = "Progress";
/// 思考块头行：运行中「Thinking · 思考正文最后完成行」（行随流更新带
/// 换行动画，ainotation 波4 #2.3）；结束「Thought · Ns」（aui "Thought
/// for Ns" 词族对位，耗时由 `format_duration_ms` 现算）
pub const LBL_THINKING_EN: &str = "Thinking";
pub const LBL_THOUGHT: &str = "Thought";
pub const LBL_DESCRIPTION: &str = "描述";
pub const LBL_ACCEPTANCE: &str = "验收标准";
pub const LBL_RUNNING: &str = "运行中";
pub const LBL_EXIT: &str = "退出";

// ---- MSG_ : 提示 / 错误 / 空态 ----
pub const MSG_EMPTY_CHAT_TITLE: &str = "开始一个新的任务";
pub const MSG_EMPTY_CHAT_DESC: &str =
    "在下方输入指令，Agent 将使用文件读写、bash 与代码编辑工具协助你完成。";
pub const MSG_THINKING: &str = "思考中";
pub const MSG_GENERATING: &str = "正在生成回复...";
pub const MSG_ATTACHMENT_READING: &str = "处理中…";
pub const MSG_NO_IMAGE_INPUT: &str = "当前模型不支持图片输入";
pub const MSG_SEARCH_SESSION: &str = "搜索会话 (⌘K)";
pub const MSG_SEARCH_PLACEHOLDER: &str = "搜索会话名称或编号...";
pub const MSG_INPUT_PLACEHOLDER: &str = "输入指令，Enter 发送，Shift+Enter 换行...";
pub const MSG_TOOL_FAILED: &str = "失败";

// ---- OPT_ : 下拉/选项文案（思考强度四档 + 统计时间范围档）----
pub const OPT_THINKING_OFF: &str = "关闭";
pub const OPT_THINKING_LIGHT: &str = "轻量";
pub const OPT_THINKING_STANDARD: &str = "标准";
pub const OPT_THINKING_DEEP: &str = "深度";
/// 双栏侧栏「统计」二级菜单的 6 档时间范围文案（id = daemon 范围 token）。
pub const OPT_STATS_RANGE_1H: &str = "最近 1 小时";
pub const OPT_STATS_RANGE_24H: &str = "最近 24 小时";
pub const OPT_STATS_RANGE_7D: &str = "最近 7 天";
pub const OPT_STATS_RANGE_30D: &str = "最近 30 天";
pub const OPT_STATS_RANGE_90D: &str = "最近 90 天";
pub const OPT_STATS_RANGE_ALL: &str = "全部";

// ---- STATUS_ : 状态徽标（任务看板 / 统计）----
pub const STATUS_ALL: &str = "全部";
pub const STATUS_IN_PROGRESS: &str = "进行中";
pub const STATUS_OPEN: &str = "待办";
pub const STATUS_DONE: &str = "已完成";
pub const STATUS_BLOCKED: &str = "已阻塞";
pub const STATUS_FAILED: &str = "失败";
pub const STATUS_ABORTED: &str = "已中止";
pub const STATUS_SUCCESS: &str = "成功";
pub const STATUS_NO_COMPARE: &str = "无可比窗口";

// ---- LBL_ : 任务看板表头 / 过滤词（H3 要求 rsx 内零 CJK 字面量；shared.rs 是规则正位）----
pub const LBL_TASKPANEL_DONE: &str = "完成";
pub const LBL_TASKPANEL_BLOCKED: &str = "阻塞";

// ---- TTL_ : 弹窗 / 页面标题 ----
pub const TTL_STATS: &str = "数据统计";
pub const TTL_ABOUT: &str = "关于 kymido";
