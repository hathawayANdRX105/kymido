//! 会话区（dsh ConversationRoot 复刻）：748px 消息列、用户右气泡 r22、
//! assistant 全宽 16/28、工作过程折叠行、浮动 composer（r22 胶囊卡）。

use dioxus::prelude::*;
use web_client::{QuestionAnswer, QuestionItem};
use web_state::types::{ChatMessage, MessagePart, PendingAttachment, StatusLine, ToolCall};

use ui_kit::button::{Button, ButtonSize, ButtonVariant};
use ui_kit::icons::{
    IconArrowUp, IconCheck, IconFolder, IconGear, IconMoon, IconPaperclip, IconPlus, IconSearch,
    IconSquareCheck, IconTerminal, IconTrash,
};
use ui_kit::{DropdownMenu, DropdownMenuItem, DropdownMenuLabel, Spinner};

use crate::utils::markdown::markdown_to_html;

/// 工具类型的配色（dsh 状态色 400 字级，ainotation #4：去掉 chip 底/边框，
/// kind 只渲染为 mono 大写小字文本）。
///
/// `job` / `terminal` 复用 brand 家族：它们和 `bash` 一样是"跑命令"，
/// 换成另一种强调色会让同一类操作在气泡上显得互不相干。三者靠 kind 文字
/// （`job` / `terminal` / `bash`，见下方渲染处）区分，不靠颜色。
fn kind_chip(kind: &str) -> &'static str {
    match kind {
        "bash" | "job" | "terminal" => "text-brand-300",
        "edit" | "write" => "text-success-2",
        "read" | "grep" | "glob" => "text-warn-2",
        "delete" => "text-danger",
        _ => "text-label-3",
    }
}

/// kind 显示名：首字母大写（bash→Bash）；match 键保持小写。
fn kind_label(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// 工具输出正文规整（ainotation 波2 #4：bash 结果曾以转义文本原样直出，
/// `# kymido\n\n` 只剩字面 \n）。只做反转义：\n \t \r \" \\ 还原成真实字符。
/// 已含真实换行的输出原样返回——此时字面 \n 多半是正则/代码示例，反转义
/// 反而坏内容。kind 暂不参与分支（bash/read 一视同仁），留在签名里供将来
/// 按 kind 定制。
pub fn format_tool_output(_kind: &str, raw: &str) -> String {
    if raw.contains('\n') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 终端观感的错误行判定（ainotation 波2 #4②）：含 error/panic/fatal/failed
/// 的行着 danger 色。故意放宽到 contains——真实日志的错误行形态太多，误染
/// 一行普通文本好过整屏无色。
fn line_is_error(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("error") || l.contains("panic") || l.contains("fatal") || l.contains("failed")
}

/// kind 前的 12px 图标（dsh GenericToolCard VARIANT_ICONS 的对应物；ui-kit
/// 字形有限，与 dsh 不同的选型在此说明）：
/// - bash/terminal/job → IconTerminal（dsh 用 IconApiOutline14；都是「跑命令」族）
/// - grep → IconSearch，glob → IconFolder（dsh：magnifier 族留 grep，folder 留 glob）
/// - read → IconFolder 复用（dsh 用 IconBrowseOutline16 眼/浏览器形；ui-kit 无对应字形，留在文件族，kind 文字区分）
/// - edit/write → IconPlus（dsh 用铅笔 IconEditOutline16；ui-kit 无铅笔/编辑字形，plus 是现有集里语义最接近「写/改」的）
/// - delete → IconTrash；think → IconMoon；tool 及未知 → IconGear
#[component]
fn KindIcon(kind: String) -> Element {
    let class = "text-label-3 shrink-0";
    match kind.as_str() {
        "bash" | "terminal" | "job" => rsx! { IconTerminal { size: 12, class } },
        "grep" => rsx! { IconSearch { size: 12, class } },
        "read" | "glob" => rsx! { IconFolder { size: 12, class } },
        "edit" | "write" => rsx! { IconPlus { size: 12, class } },
        "delete" => rsx! { IconTrash { size: 12, class } },
        "think" => rsx! { IconMoon { size: 12, class } },
        _ => rsx! { IconGear { size: 12, class } },
    }
}

const MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "qwen3-32b",
    "agnes-2.5-flash",
    "claude-opus-4-7",
    "kimi-k3",
];

const THINKING_OPTIONS: &[(&str, &str)] = &[
    ("关闭", "off"),
    ("轻量", "2k"),
    ("标准", "8k"),
    ("深度", "16k"),
];

#[component]
pub fn Chat(
    messages: Vec<ChatMessage>,
    statusline: StatusLine,
    is_streaming: bool,
    /// 浮在 composer 上方的 dock 卡片（任务看板等），由页面层传入
    dock: Option<Element>,
    /// 待决用户问题（plan-mode review 等）；None = 无卡片
    question: Option<QuestionItem>,
    /// 回答问题：(question_id, answer)。回答失败由页面层决定保留卡片
    on_answer: EventHandler<(String, QuestionAnswer)>,
    /// Send: (text, images picked in the composer and not sent yet).
    on_send: EventHandler<(String, Vec<PendingAttachment>)>,
    on_model_change: EventHandler<String>,
    on_toggle_thinking: EventHandler<()>,
    on_toggle_tasks: EventHandler<()>,
    /// 任务 dock「点外关闭」：chat 列内（滚动区 / minimap / 座位空白等非
    /// 面板区域）点击时触发。面板内部（TaskPanel 根）自行 stop_propagation
    /// 豁免，开合钮亦已豁免（见下），保持 toggle 语义（ainotation 波3 #5）
    on_outside_tasks: EventHandler<()>,
    /// 运行中点停止：中止当前 agent run
    on_abort: EventHandler<()>,
    /// T5 附件门：当前 active 模型是否声明 image 输入。false 时 composer 附件
    /// 入口置灰（不渲染 file input；附件桥 JS 缺元素自然不生效），待发卡片
    /// 仍保留移除能力（已选附件不会被静默丢弃）。
    image_input: bool,
) -> Element {
    let mut draft = use_signal(String::new);
    // Images waiting on the send button. The browser bridge writes a JSON
    // array into the hidden `#attachment-bridge` textarea (a plain `input`
    // event is all LiveView needs to see it), so no file bytes round-trip
    // through a form post.
    let mut attachments = use_signal::<Vec<PendingAttachment>>(Vec::new);
    let model_items: Vec<(String, String)> = MODELS
        .iter()
        .map(|m| (m.to_string(), m.to_string()))
        .collect();
    let thinking_items: Vec<(String, String)> = THINKING_OPTIONS
        .iter()
        .map(|(l, v)| (l.to_string(), v.to_string()))
        .collect();

    let display_messages: Vec<ChatMessage> = messages
        .iter()
        .filter(|m| {
            !m.content.is_empty()
                || !m.tool_calls.is_empty()
                || !m.parts.is_empty()
                || !m.reasoning.is_empty()
                || is_streaming
        })
        .cloned()
        .collect();

    let prompt_items: Vec<(String, String)> = display_messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| (format!("prompt-{}", m.id), m.content.clone()))
        .collect();

    // 最后一条 user 消息之后的所有 assistant 消息 = 最后一轮（ainotation #4-6）。
    // 该轮的 Work Process 保持展开，更早轮次静止即折叠。无 user 消息时整段都
    // 算最后一轮（起点取 0）。
    let last_turn_start = display_messages
        .iter()
        .rposition(|m| m.role == "user")
        .map(|i| i + 1)
        .unwrap_or(0);

    // 状态行耗时段（G5/5.6）：在飞 run 显示「当前时刻 - 开始时刻」，已结束
    // run 显示结算好的总耗时，两者都没有则为空串——空串时整段（含前导
    // 分隔符）不渲染，避免状态行出现 " · " 空档。rsx! 内禁止 let，故在
    // 此预先拼好。
    let elapsed = statusline.elapsed_label();
    let elapsed_seg = if elapsed.is_empty() {
        String::new()
    } else {
        format!(" · {elapsed}")
    };

    // 问题卡预提取 owned 数据：rsx 闭包要 'static，不能借 prop 的局部。
    // 按钮按 (qid 副本, index, label) 三元组迭代——每个闭包捕获自己那份
    let question_view = question.as_ref().map(|q| {
        (
            q.id.clone(),
            q.summary.clone(),
            q.options
                .iter()
                .map(|o| o.label.clone())
                .collect::<Vec<_>>(),
        )
    });
    let question_buttons: Vec<(String, usize, String)> = question_view
        .as_ref()
        .map(|(qid, _, labels)| {
            labels
                .iter()
                .enumerate()
                .map(|(i, l)| (qid.clone(), i, l.clone()))
                .collect()
        })
        .unwrap_or_default();

    rsx! {
        // 「点外关闭」（ainnotation 波3 #5）：事件委托——本根收 chat 列内的
        // click 并关闭任务看板；面板内部（TaskPanel 根）与任务看板开合钮
        // 各自 stop_propagation 豁免，冒泡在豁免点被截断，故不会误关。
        div { class: "relative flex-1 min-h-0 overflow-hidden",
            onclick: move |_| on_outside_tasks.call(()),
            // 单一滚动面板 = 整个聊天室
            div { class: "absolute inset-0 overflow-y-auto",
                id: "chat-scroll",
                div { class: "max-w-[780px] w-full mx-auto px-4 pt-4 pb-[220px] flex flex-col gap-4 min-h-full",
                    if display_messages.is_empty() && !is_streaming {
                        div { class: "flex-1 flex flex-col items-center justify-center gap-2.5 text-center py-10 select-none relative",
                            div { class: "absolute w-[520px] h-[220px] rounded-full bg-brand/10 blur-[110px] -z-10" }
                            div { class: "text-[26px] leading-8 font-semibold text-label", "开始一个新的任务" }
                            div { class: "text-[14px] leading-[22px] text-label-3 max-w-[420px]",
                                "在下方输入指令，Agent 将使用文件读写、bash 与代码编辑工具协助你完成。"
                            }
                        }
                    }
                    for (idx, msg) in display_messages.iter().enumerate() {
                        {
                            let is_last = idx == display_messages.len() - 1;
                            let anchor = if msg.role == "user" {
                                Some(format!("prompt-{}", msg.id))
                            } else {
                                None
                            };
                            rsx! {
                                MessageItem {
                                    key: "{msg.id}-{idx}",
                                    message: msg.clone(),
                                    // 流式指示只挂在最后一条 assistant 上
                                    streaming_tail: is_streaming && is_last && msg.role == "assistant",
                                    last_turn: idx >= last_turn_start,
                                    id: anchor,
                                }
                            }
                        }
                    }
                    div { id: "chat-scroll-anchor", class: "h-2 shrink-0" }
                }
            }

            // 左侧 minimap：每个用户 prompt 一个横条锚点（客户端 JS 聚光梯度）
            if !prompt_items.is_empty() {
                div { class: "absolute left-2 top-0 bottom-0 flex flex-col justify-center z-20",
                    id: "minimap",
                    for (anchor_id, p) in prompt_items.clone() {
                        div { class: "relative flex items-center",
                            style: "height:16px; width:56px;",
                            "data-anchor": anchor_id,
                            div { class: "rounded-full bg-subtle cursor-pointer transition-all duration-150 ease-out minimap-bar",
                                style: "height:4px; width:10px;",
                            }
                            div { class: "absolute left-14 top-1/2 -translate-y-1/2 z-30 w-[230px] max-h-[150px] overflow-hidden rounded-xl border border-binv bg-menu px-3 py-2.5 shadow-lv3 pointer-events-none",
                                "data-tip": "",
                                style: "display:none;",
                                div { class: "text-[12px] leading-5 text-label-2 whitespace-pre-wrap break-words line-clamp-6", "{p}" }
                            }
                        }
                    }
                }
            }

            // composer 悬浮座位：渐隐带 + dock + 输入卡 + 状态行（状态行移到输入卡下方，ainnotation 波3）
            div { class: "absolute bottom-0 left-0 right-0 z-30 pointer-events-none",
                div { class: "h-9 bg-gradient-to-t from-base to-transparent" }
                div { class: "mx-auto w-full max-w-[780px] px-4 pb-2 flex flex-col items-center gap-2",
                    // dock 卡片（任务看板）
                    {dock}
                    // 用户问题卡（plan-mode review）：composer 上方、dock 之下。
                    // 选项即答案：Select { index } 直发，无中间态
                    if let Some((_, qsummary, _)) = &question_view {
                        div { class: "pointer-events-auto w-full question-card rounded-[14px] border border-b1 bg-layer-1 shadow-lv2 px-4 py-3 flex flex-col gap-2.5",
                            div { class: "flex items-baseline gap-2",
                                span { class: "text-[12px] leading-4 font-medium text-brand-300 shrink-0", "计划评审" }
                                span { class: "text-[13px] leading-5 text-label-2", "{qsummary}" }
                            }
                            div { class: "flex items-center gap-2 flex-wrap",
                                for (qid_btn, i, label) in question_buttons.clone() {
                                    {
                                        let qid_click = qid_btn;
                                        rsx! {
                                            button {
                                                key: "{i}",
                                                class: "h-7 px-3 rounded-lg bg-selector hover:bg-iactive text-[12px] leading-4 text-label transition-colors",
                                                onclick: move |_| {
                                                    on_answer.call((qid_click.clone(), QuestionAnswer::Select { index: i }));
                                                },
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // 输入卡：r22 胶囊
                    // 不加 overflow-hidden：模型/思考菜单从工具行向上弹出，
                    // 裁剪会切掉卡片外的部分；圆角由卡片自身的 bg + radius 呈现
                    div { class: "pointer-events-auto w-full rounded-[22px] border border-b1 bg-input-bg shadow-lv2 flex flex-col transition-colors focus-within:border-b3",
                        // Bridge: the file picker JS writes base64 JSON here.
                        // Hidden from view, still a real textarea so
                        // LiveView's `oninput` wiring works unchanged.
                        textarea {
                            id: "attachment-bridge",
                            class: "hidden",
                            value: "",
                            oninput: move |e: FormEvent| {
                                if e.value().trim().is_empty() {
                                    return;
                                }
                                match serde_json::from_str::<Vec<PendingAttachment>>(&e.value()) {
                                    Ok(picked) => attachments.set(picked),
                                    Err(err) => eprintln!("chat: attachment bridge rejected payload: {err}"),
                                }
                            },
                        }
                        // 待发附件卡：名字 + 体积 + 移除。
                        if !attachments().is_empty() {
                            div { class: "flex flex-wrap gap-1.5 px-3 pt-2.5",
                                for (idx, att) in attachments().into_iter().enumerate() {
                                    div {
                                        class: "flex items-center gap-1.5 rounded-[10px] border border-b1 bg-layer-1 px-2 py-1 text-[12px] text-label",
                                        span { class: "max-w-[180px] truncate", "{att.name}" }
                                        span { class: "text-caption font-mono", "{att.size_bytes() / 1024} KB" }
                                        button {
                                            r#type: "button",
                                            class: "border-none bg-transparent text-caption hover:text-label cursor-pointer p-0",
                                            title: "移除",
                                            onclick: move |_| {
                                                let mut cur = attachments.write();
                                                cur.remove(idx);
                                            },
                                            span { class: "text-[13px] leading-none", "×" }
                                        }
                                    }
                                }
                            }
                        }
                        // 处理中 / 错误两态由附件桥 JS 直接填（#attachment-reading /
                        // #attachment-rejected）：读文件、类型/体积过滤都是浏览器侧的事，
                        // Rust 渲染层只负责占位，JS 按 change 事件驱动这两块 DOM。
                        div { class: "px-3",
                            span {
                                id: "attachment-reading",
                                class: "hidden text-[11px] text-caption font-mono",
                                "处理中…",
                            }
                            div {
                                id: "attachment-rejected",
                                class: "hidden flex-col gap-0.5 mt-1",
                            }
                        }
                        textarea {
                            id: "chat-input-area",
                            class: "w-full resize-none bg-transparent border-none outline-none text-[16px] leading-6 text-label placeholder:text-caption caret-brand px-4 pt-3 pb-1 min-h-[52px] max-h-[336px]",
                            placeholder: "输入指令，Enter 发送，Shift+Enter 换行...",
                            value: "{draft}",
                            oninput: move |e: FormEvent| draft.set(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                if e.key() == Key::Enter && !e.modifiers().contains(Modifiers::SHIFT) {
                                    e.prevent_default();
                                    let text = draft();
                                    if !text.trim().is_empty() && !is_streaming {
                                        on_send.call((
                                            text.trim().to_string(),
                                            std::mem::take(&mut *attachments.write()),
                                        ));
                                        draft.set(String::new());
                                    }
                                }
                            },
                        }
                        div { class: "flex items-center justify-between pl-1.5 pr-2 pb-1.5 pt-0.5",
                            div { class: "flex items-center gap-0.5",
                                // A <label for> opens the native picker without
                                // any JS, so the button stays a plain element.
                                // T5：active 模型未声明 image 输入时置灰（不渲染
                                // file input；附件桥 JS 缺元素自然不生效）。
                                if image_input {
                                    label {
                                        class: "flex items-center justify-center w-[26px] h-[26px] rounded-[8px] bg-selector hover:bg-iactive cursor-pointer",
                                        title: "添加图片附件",
                                        input {
                                            id: "attachment-input",
                                            r#type: "file",
                                            accept: "image/png,image/jpeg,image/gif,image/webp",
                                            multiple: "true",
                                            class: "hidden",
                                            onchange: move |_| {},
                                        }
                                        IconPaperclip { size: 15 }
                                    }
                                } else {
                                    span {
                                        class: "flex items-center justify-center w-[26px] h-[26px] rounded-[8px] bg-selector opacity-40 cursor-not-allowed",
                                        title: "当前模型不支持图片输入",
                                        IconPaperclip { size: 15 }
                                    }
                                }
                                Button {
                                    variant: ButtonVariant::Ghost,
                                    size: ButtonSize::IconSm,
                                    title: "任务看板",
                                    // 点外关闭（ainnotation 波3）：开合钮保持纯 toggle 语义——
                                    // stop_propagation 挡住页面级 click 委托，开→关 / 关→开
                                    // 都由 on_toggle_tasks 自己完成
                                    onclick: move |e: MouseEvent| {
                                        e.stop_propagation();
                                        on_toggle_tasks.call(());
                                    },
                                    IconSquareCheck { size: 15 }
                                }
                                MenuPicker {
                                    label: "{statusline.model}",
                                    header: "选择模型",
                                    items: model_items,
                                    active_value: statusline.model.clone(),
                                    mono: true,
                                    on_select: move |m: String| on_model_change.call(m),
                                }
                                MenuPicker {
                                    label: "思考 {statusline.thinking}",
                                    header: "思考强度",
                                    items: thinking_items,
                                    active_value: statusline.thinking.clone(),
                                    on_select: move |_| on_toggle_thinking.call(()),
                                }
                            }
                            div { class: "flex items-center gap-2.5",
                                span { class: "text-[12px] leading-5 text-caption font-mono",
                                    "{statusline.tokens_in} / {statusline.tokens_out}"
                                }
                                // Send/Stop 同位切换（dsh：主按钮运行中即停止钮）
                                if is_streaming {
                                    button {
                                        r#type: "button",
                                        // 运行态停止钮（ainnotation 波3）：28px 小圆钮 + ui-kit Spinner
                                        // （animate-spin 描边环）替代旧的 34px 白方块——更小更精致，
                                        // 且动态表达"正在跑"
                                        class: "w-[28px] h-[28px] rounded-full bg-brand text-white hover:bg-brand-hover flex items-center justify-center cursor-pointer transition-colors border-none",
                                        title: "停止",
                                        onclick: move |_| on_abort.call(()),
                                        Spinner { size: 14, class: "text-white" }
                                    }
                                } else {
                                    button {
                                        r#type: "button",
                                        class: "w-[34px] h-[34px] rounded-full bg-brand text-white hover:bg-brand-hover flex items-center justify-center cursor-pointer transition-colors border-none",
                                        title: "发送",
                                        onclick: move |_| {
                                            let text = draft();
                                            if !text.trim().is_empty() && !is_streaming {
                                                on_send.call((
                                                    text.trim().to_string(),
                                                    std::mem::take(&mut *attachments.write()),
                                                ));
                                                draft.set(String::new());
                                            }
                                        },
                                        IconArrowUp { size: 16 }
                                    }
                                }
                            }
                        }
                    }
                    // 状态行（dsh StatsLine：12/20 tertiary 居中）
                    div { class: "text-[12px] leading-5 text-label-3 text-center select-none",
                        "{statusline.model} · ↑{statusline.tokens_in} ↓{statusline.tokens_out} · ${statusline.cost_usd:.3} · context {statusline.context_pct:.0}%{elapsed_seg}"
                    }
                }
            }
        }
    }
}

/// composer 下拉选择器：ui-kit DropdownMenu 的数据驱动封装（label + header +
/// items + 选中高亮），自底部向上弹出。kit 菜单项不设「选中即关」，这里由
/// `close_req` 请求收关——Dioxus signal 每次 set 都标脏，重复 set(true) 仍会
/// 触发收关 effect，可反复选用。
#[component]
fn MenuPicker(
    label: String,
    header: String,
    items: Vec<(String, String)>,
    active_value: String,
    #[props(default = false)] mono: bool,
    on_select: EventHandler<String>,
) -> Element {
    let mut close_req = use_signal(|| false);
    let mono_class = if mono { "font-mono" } else { "" };
    rsx! {
        // DropdownMenu 根是 display:contents（无定位上下文），absolute 面板需要
        // 调用方提供 relative 锚点，否则会上浮到整个布局容器而漂位。
        div { class: "relative",
            DropdownMenu {
            content_class: "bottom-full left-0 mb-1 min-w-[190px]",
            close_signal: close_req,
            trigger: rsx! {
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    class: "{mono_class}",
                    span { "{label}" }
                }
            },
            content: rsx! {
                DropdownMenuLabel { "{header}" }
                for (item_label, item_value) in items.iter() {
                    {
                        let item_label = item_label.clone();
                        let item_value = item_value.clone();
                        let is_active = item_value == active_value;
                        rsx! {
                            DropdownMenuItem {
                                key: "{item_value}",
                                onclick: move |_| {
                                    on_select.call(item_value.clone());
                                    close_req.set(true);
                                },
                                span { class: "truncate {mono_class}", "{item_label}" }
                                if is_active {
                                    IconCheck { size: 14, class: "shrink-0 text-foreground" }
                                }
                            }
                        }
                    }
                }
            },
            }
        }
    }
}

/// 单条消息：用户右侧气泡 / assistant 按发生顺序（过程折叠 + 最终回复）。
#[component]
fn MessageItem(
    message: ChatMessage,
    streaming_tail: bool,
    /// 是否属于最后一个用户轮次（最后一个 user 之后的消息）。最后一轮的
    /// Work Process 保持展开，更早轮次静止即折叠（ainotation #4-6）。
    last_turn: bool,
    id: Option<String>,
) -> Element {
    let is_user = message.role == "user";
    let dom_id = id.unwrap_or_default();

    if is_user {
        rsx! {
            div { class: "flex flex-col items-end gap-1 w-full group",
                id: "{dom_id}",
                // 用户带的图片：先图后气泡，与 freebuff 卡片顺序一致。
                if !message.attachments.is_empty() {
                    div { class: "flex flex-wrap justify-end gap-1.5 max-w-[525px]",
                        for att in message.attachments.iter() {
                            img {
                                src: "data:{att.media_type};base64,{att.data}",
                                alt: "{att.name}",
                                title: "{att.name}",
                                class: "max-h-[180px] rounded-[14px] border border-b1 object-cover",
                            }
                        }
                    }
                }
                if !message.content.is_empty() {
                    div { class: "markdown-sm bg-bubble rounded-[22px] px-4 py-2.5 max-w-[525px]",
                        dangerous_inner_html: "{markdown_to_html(&message.content)}"
                    }
                }
                span { class: "text-[12px] leading-5 text-label-3 pr-2 opacity-0 group-hover:opacity-100 transition-opacity duration-75",
                    "{message.timestamp}"
                }
            }
        }
    } else {
        // 有序片段：为空时回退 content + tool_calls
        let parts: Vec<MessagePart> = if !message.parts.is_empty() {
            message.parts.clone()
        } else {
            let mut v = Vec::new();
            for tc in &message.tool_calls {
                v.push(MessagePart::Tool(tc.clone()));
            }
            if !message.content.is_empty() {
                v.push(MessagePart::Text(message.content.clone()));
            }
            v
        };

        let final_idx = parts
            .iter()
            .rposition(|p| matches!(p, MessagePart::Text(_)));
        let (final_text, process, has_final) = match final_idx {
            Some(fi) => {
                let ft = match &parts[fi] {
                    MessagePart::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let has_final = !ft.is_empty();
                (ft, parts[..fi].to_vec(), has_final)
            }
            None => (String::new(), parts.clone(), false),
        };
        let waiting = streaming_tail && !has_final && process.is_empty();

        rsx! {
            div { class: "flex flex-col gap-2 w-full group",
                id: "{dom_id}",
                // 思考过程（reasoning 增量）：details 原生折叠；流式中展开，
                // 无图标、无 emoji——纯文本 label
                if !message.reasoning.is_empty() {
                    details {
                        class: "select-none",
                        open: streaming_tail,
                        summary { class: "inline-flex items-center text-[12px] leading-5 text-label-3 cursor-pointer hover:text-label-2 transition-colors",
                            "思考过程"
                        }
                        div { class: "mt-1 text-[13px] leading-6 text-label-2 whitespace-pre-wrap break-words border-l border-b1 pl-3",
                            "{message.reasoning}"
                        }
                    }
                }
                if waiting {
                    // dsh turn 状态行：26px 高 shimmer
                    div { class: "h-[26px] flex items-center",
                        span { class: "shimmer-text text-[14px] font-medium", "思考中" }
                    }
                } else {
                    if !process.is_empty() {
                        ProcessBlock {
                            parts: process,
                            active: last_turn || streaming_tail,
                        }
                    }
                    if has_final {
                        div { class: "markdown-body",
                            dangerous_inner_html: "{markdown_to_html(&final_text)}"
                        }
                    }
                    if streaming_tail {
                        div { class: "flex items-center gap-2 h-[26px]",
                            Spinner {}
                            span { class: "text-[12px] leading-5 text-label-3", "正在生成回复..." }
                        }
                    }
                    // hover 元信息（时间戳）
                    span { class: "text-[12px] leading-5 text-label-3 -ml-1 h-5 opacity-0 group-hover:opacity-100 transition-opacity duration-75",
                        "{message.timestamp}"
                    }
                }
            }
        }
    }
}

/// 「Work Process」折叠块：最终回复之前的全部内容（文本 + 工具调用）。
/// 默认状态跟随 `active`（最后一轮展开、更早轮折叠 + 流式展开；ainotation
/// #4-6），用户点击头行可覆盖。chevron 已按 ainotation #2 删除。
#[component]
fn ProcessBlock(parts: Vec<MessagePart>, active: bool) -> Element {
    // None = 跟随 active；Some = 用户点过之后的显式开关
    let mut toggle = use_signal(|| None::<bool>);
    let open = toggle().unwrap_or(active);
    let count = parts.len();

    rsx! {
        div { class: "flex flex-col",
            div { class: "h-6 flex items-center gap-1.5 cursor-pointer select-none w-fit text-[14px] leading-6 text-label-2 hover:text-label",
                onclick: move |e: MouseEvent| {
                    e.stop_propagation();
                    toggle.set(Some(!open));
                },
                span { "Work Process" }
                span { class: "text-caption", "· {count}" }
            }
            if open {
                div { class: "pl-[22px] pt-1 flex flex-col gap-2",
                    for (i, p) in parts.iter().enumerate() {
                        match p {
                            MessagePart::Text(s) => rsx! {
                                div { key: "txt-{i}", class: "markdown-body",
                                    dangerous_inner_html: "{markdown_to_html(s)}"
                                }
                            },
                            MessagePart::Tool(tc) => rsx! {
                                ToolLine { key: "{tc.id}-{i}", tool: tc.clone() }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// 单次工具调用折叠行（dsh DisclosureRow：24px 头 + 展开体）。
/// 头行 = kind 图标 + 大写 kind 文本 + 「·」+ 命令标题（ainotation #4；
/// chevron 已按 #2 删除）。
#[component]
fn ToolLine(tool: ToolCall) -> Element {
    let mut open = use_signal(|| false);
    let is_err = tool.status == "error";
    let running = tool.status == "running";
    let chip = kind_chip(&tool.kind);
    let label = kind_label(&tool.kind);
    let formatted = format_tool_output(&tool.kind, &tool.detail);

    rsx! {
        div { class: "flex flex-col",
            div { class: "h-6 flex items-center gap-2 cursor-pointer select-none w-fit",
                onclick: move |e: MouseEvent| {
                    e.stop_propagation();
                    open.set(!open());
                },
                KindIcon { kind: "{tool.kind}" }
                span { class: "{chip} font-mono text-[10px] font-semibold uppercase shrink-0",
                    "{label}"
                }
                span { class: "text-[12px] leading-5 text-label-3 shrink-0", "·" }
                span { class: "font-mono text-[12px] leading-5 text-label-2 flex-1 truncate min-w-0", "{tool.title}" }
                if running {
                    Spinner {}
                }
                if is_err {
                    span { class: "text-[11px] leading-4 font-medium text-danger shrink-0", "失败" }
                }
            }
            if open() {
                div { class: "pl-[22px] pb-1 flex flex-col gap-1.5",
                    if !tool.summary.is_empty() {
                        div { class: "text-[13px] leading-5 text-label-3", "{tool.summary}" }
                    }
                    div { class: "tool-output bg-codeblock rounded-lg px-3 py-2 font-mono text-[12px] leading-[18px] text-label-2 whitespace-pre-wrap break-all",
                        for line in formatted.lines() {
                            if line.starts_with('+') && !line.starts_with("+++") {
                                span { class: "text-success-2", "{line}\n" }
                            } else if line.starts_with('-') && !line.starts_with("---") {
                                span { class: "text-danger", "{line}\n" }
                            } else if line.starts_with("@@") {
                                span { class: "text-brand", "{line}\n" }
                            } else if line.starts_with('$') {
                                // 终端观感：命令行亮于输出行
                                span { class: "text-label", "{line}\n" }
                            } else if line_is_error(line) {
                                span { class: "text-danger", "{line}\n" }
                            } else {
                                span { "{line}\n" }
                            }
                        }
                    }
                }
            }
        }
    }
}
