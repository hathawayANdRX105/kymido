//! kymido-web-state：web UI 的状态词汇表。
//!
//! - [`convert`]：daemon `session` 存储行 → UI DTO 的纯函数转换（C5.2a）
//! - [`types`]：页面/组件消费的 DTO（Session/ChatMessage/TaskItem/Stats…）
//! - [`ui_state`]：C5.1 `AgentEvent` → UI 状态转译层（纯函数）
//! - [`memory_link`]：jcode 记忆注入/抽取接缝（独有功能，C1 域）
//! - [`title_from_first_message`]：首条用户消息 → 确定性会话标题（fallback）

pub mod convert;
pub mod memory_link;
pub mod types;
pub mod ui_state;

/// 标题截断的字符上限（按 Unicode 标量值计，中文与 emoji 各占一位）。
const TITLE_MAX_CHARS: usize = 40;

/// 首条消息为空时使用的确定性占位标题（与 `会话 <ts>` 时间戳占位区分开）。
const EMPTY_TITLE_PLACEHOLDER: &str = "新会话";

/// 由首条用户消息派生确定性会话标题（对位 dsh
/// `packages/session/session-title/src/normalize.ts::fallbackSessionTitle`
/// 的截词思路——只取确定性 fallback，不调 LLM，LLM 生成是后续增强）。
///
/// 规则：取首个有内容的行 → 去掉行首 markdown 标记（`#` / `-` / `*` / `>`
/// 与空白）→ 超过 [`TITLE_MAX_CHARS`] 个字符截断并补省略号 `…`。
///
/// # 行为契约
/// - 空串 / 仅空白 / 整行只剩 markdown 标记 → [`EMPTY_TITLE_PLACEHOLDER`]。
/// - 多行只看第一行；emoji 与中文混合按 `chars()` 计数，不按字节。
pub fn title_from_first_message(text: &str) -> String {
    // ponytail: 首句即标题——多条消息由调用方保证只传第一条，这里只取首行。
    let Some(body) = text.lines().find(|line| !line.trim().is_empty()) else {
        return EMPTY_TITLE_PLACEHOLDER.to_string();
    };
    // 行首 markdown 标记可重复出现（`## `、`> - `），一次性剥掉。
    let stripped = body
        .trim()
        .trim_start_matches(|c| matches!(c, '#' | '-' | '*' | '>' | ' ' | '\t'));
    if stripped.is_empty() {
        return EMPTY_TITLE_PLACEHOLDER.to_string();
    }
    if stripped.chars().count() > TITLE_MAX_CHARS {
        // chars() 而非 bytes()：中文 / emoji 按字符截，不会切半成乱码。
        let mut title: String = stripped.chars().take(TITLE_MAX_CHARS).collect();
        title.push('…');
        title
    } else {
        stripped.to_string()
    }
}

/// 会话占位标题判定（known-issue 2 的刷新修复用）：`会话 <纯数字>` 是
/// 侧栏新建瞬间的时间戳占位（`create_session_in` 的 `会话 {ts % 1e6}`，
/// 旧数据里有全量 ts 变体）。首条消息标题落库失败后会话长期停在占位——
/// 页面挂载时据此识别并补派生。用户把标题改成「会话 xxx」字样但尾段
/// 非纯数字时判 false，不触发误修。
pub fn is_placeholder_title(title: &str) -> bool {
    let rest = title.strip_prefix("会话 ").unwrap_or("");
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}
