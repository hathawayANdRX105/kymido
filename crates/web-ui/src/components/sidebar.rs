//! 侧栏（dsh SidebarRoot 复刻）：logo 行 + 新会话钮 + 项目/会话树 +
//! 底部「数据统计 / 设置」触发行；折叠 rail 56px。

use dioxus::prelude::*;
use std::collections::{HashMap, HashSet};
use web_state::types::{Session, SessionStatus, WorkspaceSpace};

use crate::shared as sh;
use ui_kit::button::{Button, ButtonSize, ButtonVariant};
use ui_kit::icons::{
    IconChartBar, IconFolder, IconPanelLeft, IconPlus, IconSearch, IconSettings, IconTrash,
};

use super::collapsed_rail::CollapsedRail;
use super::session_row::SessionRow;

/// 品牌字标（Logo 行）：kymido + 品牌蓝圆点。品牌字标属业务身份，不进 ui-kit。
#[component]
fn Wordmark() -> Element {
    rsx! {
        span { class: "flex items-baseline gap-1.5 select-none",
            span { class: "text-[18px] leading-6 font-semibold tracking-[0.04em] text-label", "kymido" }
            span { class: "w-1.5 h-1.5 rounded-full bg-brand translate-y-[-2px]" }
        }
    }
}

pub(crate) fn status_dot_class(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Active => "bg-brand",
        SessionStatus::Idle => "bg-dim",
        // 中断/半开 run 与归档同走 danger 色（WP-C）
        SessionStatus::Archived | SessionStatus::Aborted => "bg-danger/70",
    }
}

#[component]
pub fn Sidebar(
    spaces: Vec<WorkspaceSpace>,
    space_sessions: HashMap<String, Vec<Session>>,
    active_id: String,
    on_select: EventHandler<String>,
    on_select_space: EventHandler<String>,
    on_create: EventHandler<String>,
    on_delete_session: EventHandler<String>,
    on_delete_space: EventHandler<String>,
    collapsed: bool,
    on_toggle: EventHandler<()>,
    width: usize,
    on_resize_start: EventHandler<i32>,
    on_preset_start: EventHandler<i32>,
    on_expand: EventHandler<()>,
    on_open_search: EventHandler<()>,
    on_open_stats: EventHandler<()>,
    on_open_settings: EventHandler<()>,
) -> Element {
    // 项目默认全部展开
    let mut expanded_spaces = use_signal(|| {
        spaces
            .iter()
            .map(|s| s.path.clone())
            .collect::<HashSet<_>>()
    });

    // 多个 move 闭包要读「第一个项目」——提前取好，各自克隆
    let first_space_path = spaces.first().map(|s| s.path.clone());
    let first_path_logo = first_space_path.clone();
    let first_path_new_chat = first_space_path;

    let aside_style = if collapsed {
        "width:56px;min-width:56px;".to_string()
    } else {
        format!("width:{width}px;min-width:{width}px;")
    };

    rsx! {
        aside { class: "relative h-full bg-sidebar border-r border-b1 flex flex-col shrink-0 select-none",
            style: "{aside_style}",
            if collapsed {
                CollapsedRail {
                    spaces: spaces.clone(),
                    space_sessions: space_sessions.clone(),
                    active_id: active_id.clone(),
                    on_select: on_select,
                    on_toggle: on_toggle,
                    on_expand: on_expand,
                    on_open_stats: on_open_stats,
                    on_open_settings: on_open_settings,
                    on_preset_start: on_preset_start,
                }
            } else {
                div { class: "flex-1 min-h-0 flex flex-col px-3 pt-1.5",
                    // Logo 行：品牌字标 + 折叠钮
                    div { class: "h-[52px] flex items-center justify-between pl-1 pr-0",
                        button {
                            class: "cursor-pointer bg-transparent border-none p-0",
                            title: sh::BTN_NEW_SESSION,
                            onclick: move |_| {
                                if let Some(path) = first_path_logo.clone() {
                                    on_create.call(path);
                                }
                            },
                            Wordmark {}
                        }
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::IconSm,
                            title: sh::BTN_COLLAPSE_SIDEBAR,
                            onclick: move |_| on_toggle.call(()),
                            IconPanelLeft { size: 16 }
                        }
                    }
                    // 新会话按钮：h38 r12 elevated + 边框
                    button {
                        class: "h-[38px] mx-0.5 mb-2 px-4 rounded-xl border border-b2 bg-layer-2 hover:bg-layer-3 hover:border-b3 flex items-center justify-center gap-1.5 text-[14px] leading-[22px] text-label-2 hover:text-label transition-colors cursor-pointer",
                        onclick: move |_| {
                            if let Some(path) = first_path_new_chat.clone() {
                                on_create.call(path);
                            }
                        },
                        IconPlus { size: 15, class: "text-label-3" }
                        span { {sh::BTN_NEW_SESSION} }
                    }
                    // 区头：标题 + 搜索（⌘K 快速切换入口）
                    div { class: "h-9 flex items-center justify-between pl-1 pr-0.5",
                        span { class: "text-[12px] leading-4 text-caption", {sh::LBL_SESSIONS} }
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::IconSm,
                            class: "nav-search-bar",
                            title: "搜索会话 (⌘K)",
                            onclick: move |_| on_open_search.call(()),
                            IconSearch { size: 15 }
                        }
                    }
                    // 项目/会话树
                    div { class: "flex-1 min-h-0 overflow-y-auto no-scrollbar flex flex-col pb-2",
                        for space in spaces {
                            {
                                let space_path = space.path.clone();
                                let space_path_create = space.path.clone();
                                let space_path_delete = space.path.clone();
                                let space_path_toggle = space.path.clone();
                                let is_open = expanded_spaces().contains(&space_path);
                                let sessions_for_space = space_sessions
                                    .get(&space.path)
                                    .cloned()
                                    .unwrap_or_default();
                                let count = sessions_for_space.len();
                                rsx! {
                                    // 项目行 h34
                                    div { class: "group h-[34px] mx-0 px-2 rounded-lg flex items-center gap-2 hover:bg-ihover cursor-pointer transition-colors",
                                        onclick: move |_| {
                                            let mut set = expanded_spaces.write();
                                            if set.contains(&space_path_toggle) {
                                                set.remove(&space_path_toggle);
                                            } else {
                                                set.insert(space_path_toggle.clone());
                                            }
                                            drop(set);
                                            on_select_space.call(space_path.clone());
                                        },
                                        IconFolder { size: 16, class: "shrink-0 text-label-3" }
                                        span { class: "text-[14px] leading-5 text-label truncate min-w-0 flex-1", "{space.name}" }
                                        button {
                                            class: "shrink-0 flex items-center justify-center w-4 h-4 text-label-3 hover:text-label opacity-40 group-hover:opacity-100 transition-opacity cursor-pointer bg-transparent border-none",
                                            title: sh::BTN_NEW_CHAT_IN_SPACE,
                                            onclick: move |e: MouseEvent| {
                                                e.stop_propagation();
                                                on_create.call(space_path_create.clone());
                                            },
                                            IconPlus { size: 13 }
                                        }
                                        span { class: "text-[12px] leading-5 text-label-3 tabular-nums", "{count}" }
                                        // 移除项目钮：hover 行才出现，排在行最右
                                        button {
                                            class: "shrink-0 flex items-center justify-center w-4 h-4 text-label-3 hover:text-danger opacity-40 group-hover:opacity-100 transition-opacity cursor-pointer bg-transparent border-none",
                                            title: sh::BTN_REMOVE_SPACE,
                                            onclick: move |e: MouseEvent| {
                                                e.stop_propagation();
                                                on_delete_space.call(space_path_delete.clone());
                                            },
                                            IconTrash { size: 13 }
                                        }
                                    }
                                    // 会话行 h32，缩进 22px；树内子会话再逐层缩进
                                    if is_open {
                                        div { class: "pl-[22px] flex flex-col gap-px pb-1",
                                            for (session, depth) in group_sessions(&sessions_for_space) {
                                                SessionRow {
                                                    key: "{session.id}",
                                                    session: session.clone(),
                                                    depth,
                                                    active: session.id == active_id,
                                                    on_select: on_select,
                                                    on_delete: on_delete_session,
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // 底部：数据统计 / 设置
                div { class: "px-3 pb-2 flex flex-col gap-0.5",
                    div { class: "h-[42px] px-2.5 rounded-xl flex items-center gap-2.5 text-[14px] leading-[22px] text-label-2 hover:bg-ihover hover:text-label cursor-pointer transition-colors {ui_kit::icons::ANIM_SCOPE}",
                        onclick: move |_| on_open_stats.call(()),
                        IconChartBar { size: 16, class: "text-label-3" }
                        span { {sh::TTL_STATS} }
                    }
                    div { class: "h-[42px] px-2.5 rounded-xl flex items-center gap-2.5 text-[14px] leading-[22px] text-label-2 hover:bg-ihover hover:text-label cursor-pointer transition-colors {ui_kit::icons::ANIM_SCOPE}",
                        onclick: move |_| on_open_settings.call(()),
                        IconSettings { size: 16, class: "text-label-3" }
                        span { {sh::LBL_SETTINGS} }
                    }
                }
                // 右缘拖拽把手
                div {
                    class: "absolute top-0 right-0 h-full w-0.5 cursor-col-resize hover:bg-brand/20 transition-colors",
                    onmousedown: move |e: MouseEvent| on_resize_start.call(e.client_coordinates().x as i32),
                }
            }
        }
    }
}

/// 把一个 space 内的扁平会话列表按 `parent_id` 组成渲染树：父在前、子紧随
/// 其后并逐层加深，返回 `(会话引用, 层级)`。`parent_id` 是无 FK 的自由文本，
/// 三条边界：父不在本列表（被删 / 在别的 space）→ 当根；成环（a→b→a）
/// 由 visited 集合截断并在第二趟当根兜底；层级超深时缩进封顶但**节点照常
/// 渲染**——深链不该让会话从侧栏消失。绝不死循环。渲染顺序沿原列表顺序，
/// 与 page-workspace「子插在父之后」对齐。
///
/// `pub` 是为了让 `tests/group_sessions.rs` 锁住这几条边界不变式——它跑在
/// 渲染路径上，一旦死循环或层级算错，表现是侧栏卡死/错位而不是编译失败。
pub fn group_sessions(sessions: &[Session]) -> Vec<(&Session, usize)> {
    const MAX_DEPTH: usize = 16;

    let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    // 只认父也在本列表内的边；父不在 → 该会话是根
    let mut children: HashMap<&str, Vec<&Session>> = HashMap::new();
    for s in sessions {
        if let Some(pid) = s.parent_id.as_deref()
            && by_id.contains_key(pid)
        {
            children.entry(pid).or_default().push(s);
        }
    }

    let mut visited: HashSet<&str> = HashSet::new();
    let mut out: Vec<(&Session, usize)> = Vec::new();
    let is_root = |s: &Session| match s.parent_id.as_deref() {
        Some(pid) => !by_id.contains_key(pid),
        None => true,
    };
    // 两趟：第一趟只处理根，第二趟兜底成环残余（a↔b 互相指向时二者都不是根）
    for pass in 0..2u8 {
        for s in sessions {
            if visited.contains(s.id.as_str()) {
                continue;
            }
            if pass == 0 && !is_root(s) {
                continue;
            }
            let mut stack: Vec<(&Session, usize)> = vec![(s, 0)];
            while let Some((node, depth)) = stack.pop() {
                if !visited.insert(node.id.as_str()) {
                    continue;
                }
                out.push((node, depth));
                if let Some(kids) = children.get(node.id.as_str()) {
                    // 反序入栈，弹出时保持原列表顺序。超过 MAX_DEPTH 的后代
                    // 仍然渲染，只是缩进封顶——深链不该让会话从侧栏消失。
                    for child in kids.iter().rev() {
                        stack.push((child, (depth + 1).min(MAX_DEPTH)));
                    }
                }
            }
        }
    }
    out
}

/// 子会话逐层缩进 18px；根会话由外层容器的 `pl-[22px]` 统一缩进，故
/// depth 0 不再叠加。层级封顶 3 层，更深的嵌套不再加宽，避免把行内容
/// 挤出侧栏可视区。
pub(crate) fn depth_indent_class(depth: usize) -> &'static str {
    match depth {
        0 => "",
        1 => "pl-[18px]",
        2 => "pl-[36px]",
        _ => "pl-[54px]",
    }
}
