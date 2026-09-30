//! 会话区（dsh ConversationRoot 复刻）：748px 消息列、用户右气泡 r22、
//! assistant 全宽 16/28、工作过程折叠行、浮动 composer（r22 胶囊卡）。

use dioxus::prelude::*;
use web_client::{QuestionAnswer, QuestionItem};
use web_state::types::{ChatMessage, PendingAttachment, StatusLine};

use crate::shared as sh;
use ui_kit::Spinner;
use ui_kit::icons::{
    IconArrowUp, IconFolder, IconGear, IconMoon, IconPaperclip, IconPlus, IconSearch,
    IconSquareCheck, IconTerminal, IconTrash,
};

use super::menu_picker::MenuPicker;
use super::message::MessageItem;

/// 工具类型的配色（dsh 状态色 400 字级，ainotation #4：去掉 chip 底/边框，
/// kind 只渲染为 mono 大写小字文本）。
///
/// `job` / `terminal` 复用 brand 家族：它们和 `bash` 一样是"跑命令"，
/// 换成另一种强调色会让同一类操作在气泡上显得互不相干。三者靠 kind 文字
/// （`job` / `terminal` / `bash`，见下方渲染处）区分，不靠颜色。
pub(crate) fn kind_chip(kind: &str) -> &'static str {
    match kind {
        "bash" | "job" | "terminal" => "text-brand-300",
        "edit" | "write" => "text-success-2",
        "read" | "grep" | "glob" => "text-warn-2",
        "delete" => "text-danger",
        _ => "text-label-3",
    }
}

/// kind 显示名：首字母大写（bash→Bash）；match 键保持小写。
pub(crate) fn kind_label(kind: &str) -> String {
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
pub(crate) fn line_is_error(line: &str) -> bool {
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
pub(crate) fn KindIcon(kind: String) -> Element {
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
    (sh::OPT_THINKING_OFF, "off"),
    (sh::OPT_THINKING_LIGHT, "2k"),
    (sh::OPT_THINKING_STANDARD, "8k"),
    (sh::OPT_THINKING_DEEP, "16k"),
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
    // 发送失败提示段（known-issue 1）：last_error 非空时追加在耗时段之后，
    // 空串不渲染（同 elapsed_seg 的「无真实来源不编造」纪律）。
    let error_seg = if statusline.last_error.is_empty() {
        String::new()
    } else {
        format!(" · {}", statusline.last_error)
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
                            div { class: "text-[26px] leading-8 font-semibold text-label", {sh::MSG_EMPTY_CHAT_TITLE} }
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
                                span { class: "text-[12px] leading-4 font-medium text-brand-300 shrink-0", {sh::LBL_PLAN_REVIEW} }
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
                    div { class: "pointer-events-auto w-full rounded-[24px] border border-b1 bg-input-bg shadow-lv2 p-2.5 flex flex-col gap-1.5 transition-colors focus-within:border-b3",
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
                            div { class: "flex flex-wrap gap-1.5 px-1 pt-0.5",
                                for (idx, att) in attachments().into_iter().enumerate() {
                                    div {
                                        class: "flex items-center gap-1.5 rounded-[10px] border border-b1 bg-layer-1 px-2 py-1 text-[12px] text-label",
                                        span { class: "max-w-[180px] truncate", "{att.name}" }
                                        span { class: "text-caption font-mono", "{att.size_bytes() / 1024} KB" }
                                        button {
                                            r#type: "button",
                                            class: "border-none bg-transparent text-caption hover:text-label cursor-pointer p-0",
                                            title: sh::BTN_REMOVE,
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
                        div { class: "px-1",
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
                            class: "w-full resize-none bg-transparent border-none outline-none text-[15px] leading-6 text-label placeholder:text-caption caret-brand px-3 pt-1.5 pb-1 min-h-[44px] max-h-[336px]",
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
                        div { class: "flex items-center justify-between px-1 py-0.5",
                            // aui ComposerToolbar / ComposerActions：左动作列 gap-1.5
                            div { class: "flex items-center gap-1.5",
                                // A <label for> opens the native picker without
                                // any JS, so the button stays a plain element.
                                // T5：active 模型未声明 image 输入时置灰（不渲染
                                // file input；附件桥 JS 缺元素自然不生效）。
                                if image_input {
                                    label {
                                        class: "flex items-center justify-center w-[32px] h-[32px] rounded-full text-label-2 hover:bg-selector transition-[background-color,color,scale] duration-150 active:scale-[0.96] cursor-pointer",
                                        title: sh::BTN_ADD_IMAGE,
                                        input {
                                            id: "attachment-input",
                                            r#type: "file",
                                            accept: "image/png,image/jpeg,image/gif,image/webp",
                                            multiple: "true",
                                            class: "hidden",
                                            onchange: move |_| {},
                                        }
                                        IconPaperclip { size: 16 }
                                    }
                                } else {
                                    span {
                                        class: "flex items-center justify-center w-[32px] h-[32px] rounded-full text-label-2 opacity-40 cursor-not-allowed",
                                        title: sh::MSG_NO_IMAGE_INPUT,
                                        IconPaperclip { size: 16 }
                                    }
                                }
                                // aui ghost 按钮对位：32px 圆形 ghost（纯元素，保留 toggle 语义）
                                button {
                                    r#type: "button",
                                    class: "flex items-center justify-center w-[32px] h-[32px] rounded-full text-label-2 hover:bg-selector transition-[background-color,color,scale] duration-150 active:scale-[0.96] cursor-pointer border-none bg-transparent",
                                    title: sh::BTN_TASK_PANEL,
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
                                    header: sh::LBL_PICK_MODEL,
                                    items: model_items,
                                    active_value: statusline.model.clone(),
                                    mono: true,
                                    on_select: move |m: String| on_model_change.call(m),
                                }
                                MenuPicker {
                                    label: "思考 {statusline.thinking}",
                                    header: sh::LBL_THINKING_STRENGTH,
                                    items: thinking_items,
                                    active_value: statusline.thinking.clone(),
                                    on_select: move |_| on_toggle_thinking.call(()),
                                }
                            }
                                // aui ComposerSend 三态：idle（输入空置灰）/ ready（可发）/
                                // streaming（停止钮，ui-kit Spinner）；token 计数只在卡下状态行
                                if is_streaming {
                                    button {
                                        r#type: "button",
                                        class: "w-[32px] h-[32px] rounded-full bg-brand text-white hover:bg-brand-hover flex items-center justify-center cursor-pointer transition-[opacity,scale] duration-150 active:scale-[0.96] border-none",
                                        title: sh::BTN_STOP,
                                        onclick: move |_| on_abort.call(()),
                                        Spinner { size: 14, class: "text-white" }
                                    }
                                } else if draft().trim().is_empty() {
                                    button {
                                        r#type: "button",
                                        class: "w-[32px] h-[32px] rounded-full bg-selector text-label-3 flex items-center justify-center cursor-default border-none",
                                        title: sh::BTN_SEND,
                                        IconArrowUp { size: 16 }
                                    }
                                } else {
                                    button {
                                        r#type: "button",
                                        class: "w-[32px] h-[32px] rounded-full bg-brand text-white hover:bg-brand-hover flex items-center justify-center cursor-pointer transition-[opacity,scale] duration-150 active:scale-[0.96] border-none",
                                        title: sh::BTN_SEND,
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
                    // 状态行（dsh StatsLine：12/20 tertiary 居中）
                    div { class: "text-[12px] leading-5 text-label-3 text-center select-none",
                        "{statusline.model} · ↑{statusline.tokens_in} ↓{statusline.tokens_out} · ${statusline.cost_usd:.3} · context {statusline.context_pct:.0}%{elapsed_seg}{error_seg}"
                    }
                }
            }
        }
    }
    }
}
