//! 折叠 rail（56px，自 sidebar.rs 拆出）：图标列 + CSS 悬停信息面板。

use dioxus::prelude::*;
use web_state::types::{Session, WorkspaceSpace};

use crate::shared as sh;
use ui_kit::icons::{IconChartBar, IconGear, IconPanelLeft, IconPlus};

use std::collections::HashMap;

use super::sidebar::status_dot_class;

/// 折叠 rail（56px）：图标列 + CSS 悬停信息面板。
#[component]
pub(crate) fn CollapsedRail(
    spaces: Vec<WorkspaceSpace>,
    space_sessions: HashMap<String, Vec<Session>>,
    active_id: String,
    on_select: EventHandler<String>,
    on_toggle: EventHandler<()>,
    on_expand: EventHandler<()>,
    on_open_stats: EventHandler<()>,
    on_open_settings: EventHandler<()>,
    on_preset_start: EventHandler<i32>,
) -> Element {
    rsx! {
        div { class: "pt-3 pb-2 px-[10px] flex flex-col items-center gap-1.5 h-full",
            button {
                class: "w-9 h-9 flex items-center justify-center rounded-full text-label-3 hover:bg-ihover hover:text-label-2 transition-colors cursor-pointer bg-transparent",
                title: sh::BTN_EXPAND_SIDEBAR,
                onclick: move |_| on_toggle.call(()),
                IconPanelLeft { size: 16 }
            }
            button {
                class: "w-9 h-9 flex items-center justify-center rounded-full border border-b2 text-label-3 hover:bg-ihover hover:text-label-2 transition-colors cursor-pointer bg-transparent",
                title: sh::BTN_NEW_SESSION,
                onclick: move |_| on_expand.call(()),
                IconPlus { size: 15 }
            }
            div { class: "my-1 w-5 h-px bg-b2" }
            div { class: "flex-1 min-h-0 overflow-y-auto no-scrollbar w-full flex flex-col items-center gap-1",
                for space in spaces {
                    {
                        let sp_name = space.name.clone();
                        let sp_sessions = space_sessions.get(&space.path).cloned().unwrap_or_default();
                        rsx! {
                            for s in sp_sessions {
                                {
                                    let sid = s.id.clone();
                                    let stitle = s.title.clone();
                                    let slast = s.last_active.clone();
                                    let is_selected = s.id == active_id;
                                    let dot = status_dot_class(&s.status);
                                    rsx! {
                                        div { class: "relative group shrink-0",
                                            button {
                                                class: if is_selected {
                                                    "w-9 h-9 flex items-center justify-center rounded-full cursor-pointer bg-ihover transition-colors bg-transparent border-none"
                                                } else {
                                                    "w-9 h-9 flex items-center justify-center rounded-full cursor-pointer hover:bg-ihover transition-colors bg-transparent border-none"
                                                },
                                                onclick: move |_| on_select.call(sid.clone()),
                                                span { class: "w-2.5 h-2.5 rounded-full {dot}" }
                                            }
                                            // 悬停信息面板（纯 CSS）
                                            div { class: "absolute left-full top-1/2 -translate-y-1/2 ml-2 z-50 w-52 rounded-xl border border-binv bg-menu px-3 py-2 shadow-lv3 opacity-0 pointer-events-none group-hover:opacity-100 transition-opacity duration-150",
                                                div { class: "text-[13px] leading-5 font-medium text-label truncate", "{stitle}" }
                                                div { class: "mt-1 flex items-center justify-between gap-2",
                                                    span { class: "text-[11px] text-label-3 truncate", "{sp_name}" }
                                                    span { class: "text-[11px] text-label-3 font-mono shrink-0", "{slast}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // 底部「数据统计 / 设置」：shrink-0 防止窗口偏矮时被会话列表挤裁
            div { class: "shrink-0 flex flex-col items-center gap-1.5",
                div {
                    class: "h-px w-6 bg-b2 cursor-ew-resize hover:bg-brand",
                    title: sh::MSG_SIDEBAR_WIDTH_PRESET,
                    onmousedown: move |e: MouseEvent| on_preset_start.call(e.client_coordinates().x as i32),
                }
                button {
                    class: "w-9 h-9 flex items-center justify-center rounded-full text-label-3 hover:bg-ihover hover:text-label-2 transition-colors cursor-pointer bg-transparent",
                    title: sh::TTL_STATS,
                    onclick: move |_| on_open_stats.call(()),
                    IconChartBar { size: 16 }
                }
                button {
                    class: "w-9 h-9 flex items-center justify-center rounded-full text-label-3 hover:bg-ihover hover:text-label-2 transition-colors cursor-pointer bg-transparent",
                    title: sh::LBL_SETTINGS,
                    onclick: move |_| on_open_settings.call(()),
                    IconGear { size: 16 }
                }
            }
        }
    }
}
