//! Dioxus LiveView Web UI for kymido — dsh 风格。
//!
//! 无顶栏（dsh 无 topbar）：侧栏承载全部入口（新会话 / 搜索 ⌘K /
//! 数据统计 / 设置弹窗）。页面数据走 daemon RPC（`stats.summary` / `runs_for_session` / `list_sessions`），无 daemon 时空态。

use dioxus::prelude::*;
use web_client::llm;
use web_ui::Workspace;

#[component]
pub fn App() -> Element {
    let mut runtime_config = use_signal(llm::LlmRuntimeConfig::load_from_system);
    let css_content = include_str!(concat!(env!("OUT_DIR"), "/tailwind.gen.css")).to_string();

    rsx! {
        style { "{css_content}" }
        Workspace {
            config: runtime_config(),
            on_update_config: move |cfg| runtime_config.set(cfg),
        }
    }
}

/// Launch the interactive Dioxus LiveView server on http://127.0.0.1:8026.
pub async fn launch() {
    let port = std::env::var("PORT")
        .or_else(|_| std::env::var("KYMIDO_WEB_PORT"))
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8026);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let view = dioxus_liveview::LiveViewPool::new();
    let glue = dioxus_liveview::interpreter_glue("/ws");
    let css = include_str!(concat!(env!("OUT_DIR"), "/tailwind.gen.css")).to_string();

    // Ainotation 标注 bundle（`npm run aino` 生成，源 bin/web/ainotation-entry.ts）：
    // 仅 dev 构建内联进 index——release 无痕。同步链：页面 SDK → 本地桥
    // （scripts/ainotation-bridge.mjs :44091 签 grant）→ MCP service。`</script`
    // 转义防内联脚本截断。用法：起 service + kymido 桥（见 AGENTS.md 标注栈节）。
    #[cfg(debug_assertions)]
    let aino_script = format!(
        "<script>{}</script>",
        include_str!("../assets/ainotation/ainotation.iife.js").replace("</script", "<\\/script")
    );
    #[cfg(not(debug_assertions))]
    let aino_script = String::new();

    let index_html = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>kymido</title>
    <style>
{css}
    </style>
    {aino_script}
</head>
<body>
    <div id="main"></div>
    {glue}
    <script>
    (function() {{
        function scrollToBottom(smooth) {{
            const chatEl = document.getElementById("chat-scroll");
            if (chatEl) {{
                if (smooth === false) {{
                    // 显式硬跳：页面加载 / 用户提交（新内容必须立刻可见）
                    chatEl.scrollTop = chatEl.scrollHeight;
                }} else {{
                    // 丝滑跟随：aui Viewport 的 auto-follow 对位——流式内容
                    // 增长时平滑滚到底（浏览器对同向 smooth 目标可中断重定标）
                    chatEl.scrollTo({{ top: chatEl.scrollHeight, behavior: "smooth" }});
                }}
            }}
        }}

        function scrollToId(id) {{
            const el = document.getElementById(id);
            if (!el) return;
            el.scrollIntoView({{ behavior: "smooth", block: "start" }});
        }}

        // Minimap: wheel gradient + scroll-spy highlight + hover popover +
        // click-to-jump, all client-side. Bars carry a `data-anchor` (the prompt
        // element id); hovering a bar floats a popover with that prompt's text
        // (server-rendered [data-tip] sibling, pointer-events:none so it never
        // blocks the next hover); clicking smooth-scrolls #chat-scroll to the
        // prompt. A discrete 3-level
        // gradient is centered on a reference index (scroll position at rest, the
        // cursor while hovering): only the center bar + the two on each side (5 bars
        // total) are emphasized via LENGTH + BRIGHTNESS — center longest/brightest,
        // ±1 medium, ±2 short; every other bar stays one uniform width. No color fill.
        function setupMinimap() {{
            var mm = document.getElementById('minimap');
            if (!mm) return;
            var scrollEl = document.getElementById('chat-scroll');
            // Namespaced, persistent shared state (survives repeated setupMinimap calls
            // from the MutationObserver) to avoid polluting globals and stale caches.
            var NS = window.__mm = window.__mm || {{}};
            if (typeof NS.ref !== 'number') NS.ref = 0;
            // Cached DOM queries; invalidated whenever the chat DOM changes.
            function invalidate() {{ NS.barsCache = null; NS.centers = null; }}
            function getBars() {{
                if (!NS.barsCache) NS.barsCache = Array.prototype.slice.call(mm.querySelectorAll('[data-anchor]'));
                return NS.barsCache;
            }}
            function getCenters() {{
                if (!NS.centers) {{
                    var bs = getBars();
                    NS.centers = bs.map(function(b) {{
                        var r = b.getBoundingClientRect();
                        return r.top + r.height / 2;
                    }});
                }}
                return NS.centers;
            }}
            // 3-step prominence centered on index c: 0=center, 1=adjacent, 2=outer, else=uniform.
            function applyGradient(c) {{
                NS.ref = c;
                var bs = getBars();
                for (var i = 0; i < bs.length; i++) {{
                    var bar = bs[i].querySelector('.minimap-bar');
                    if (!bar) continue;
                    var off = Math.abs(i - c);
                    var width, op;
                    if (off === 0)      {{ width = 28; op = 1.0; }}
                    else if (off === 1) {{ width = 20; op = 0.72; }}
                    else if (off === 2) {{ width = 14; op = 0.5; }}
                    else                {{ width = 10; op = 0.34; }}
                    bar.style.width = width + 'px';
                    bar.style.opacity = op.toFixed(3);
                    // 统一条底（渲染层 bg-label 近白通吃所有横条，不做逐档换色；
                    // 用户批注：minimap 横条统一白色底，聚光只走长度+亮度）
                }}
            }}
            function nearestIndex(y) {{
                var cs = getCenters();
                var best = 0, bestD = Infinity;
                for (var i = 0; i < cs.length; i++) {{
                    var d = Math.abs(y - cs[i]);
                    if (d < bestD) {{ bestD = d; best = i; }}
                }}
                return best;
            }}
            function scrollToIdx(idx) {{
                var bs = getBars();
                if (idx < 0 || idx >= bs.length) return;
                var anchor = bs[idx].getAttribute('data-anchor');
                if (anchor) scrollToId(anchor);
                applyGradient(idx);
            }}
            // Bottom detection with a tolerance instead of a brittle -4 magic number.
            var BOTTOM_TOLERANCE = 32;
            function isAtBottom() {{
                return scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight <= BOTTOM_TOLERANCE;
            }}
            function scrollIndex() {{
                var bs = getBars();
                if (!bs.length) return 0;
                if (isAtBottom()) return bs.length - 1;
                var top = scrollEl.getBoundingClientRect().top;
                var active = 0;
                for (var i = 0; i < bs.length; i++) {{
                    var a = bs[i].getAttribute('data-anchor');
                    var el = a ? document.getElementById(a) : null;
                    if (!el) continue;
                    if (el.getBoundingClientRect().top - top <= 120) active = i;
                }}
                return active;
            }}
            // rAF-throttled paint: hot paths (mousemove/scroll) write the DOM at most
            // once per frame, and only when the target index actually changes. This
            // avoids layout thrashing from per-event getBoundingClientRect reads.
            var rafPending = false;
            var pending = null;
            var lastPainted = null;
            function schedule(c) {{
                if (c === lastPainted && pending === null) return;
                pending = c;
                if (rafPending) return;
                rafPending = true;
                requestAnimationFrame(function() {{
                    rafPending = false;
                    if (pending === null) return;
                    applyGradient(pending);
                    lastPainted = pending;
                    pending = null;
                }});
            }}
            if (!NS.wired) {{
                NS.wired = true;
                mm.addEventListener('wheel', function(e) {{
                    e.preventDefault();
                    var bs = getBars();
                    if (!bs.length) return;
                    var cur = (typeof NS.ref === 'number') ? NS.ref : scrollIndex();
                    var dir = e.deltaY > 0 ? 1 : -1;
                    var nxt = Math.max(0, Math.min(bs.length - 1, cur + dir));
                    scrollToIdx(nxt);
                }}, {{ passive: false }});
                // hover：浮出 data-tip popover（服务端已渲染好 prompt 文本）。
                // 注意取条自身内部的 tip——data-anchor 与 data-tip 同在一个
                // 条容器里，往父级查会永远命中第一条的 popover。
                mm.addEventListener('mouseover', function(e) {{
                    var bar = e.target.closest('[data-anchor]');
                    if (!bar) return;
                    var tip = bar.querySelector('[data-tip]');
                    if (tip) tip.style.display = 'block';
                }});
                mm.addEventListener('mouseout', function(e) {{
                    var bar = e.target.closest('[data-anchor]');
                    if (!bar) return;
                    var to = e.relatedTarget;
                    if (to && bar.contains(to)) return;
                    var tip = bar.querySelector('[data-tip]');
                    if (tip) tip.style.display = 'none';
                }});
                // click：平滑滚到该条锚点对应的用户 prompt（ainotation 波2 #3）。
                mm.addEventListener('click', function(e) {{
                    var bar = e.target.closest('[data-anchor]');
                    if (!bar) return;
                    var idx = getBars().indexOf(bar);
                    if (idx >= 0) scrollToIdx(idx);
                }});
                // Cache bar centers when the hover begins; subsequent snapping reads the
                // cache (no per-event getBoundingClientRect → no layout thrashing).
                mm.addEventListener('mouseenter', function() {{ NS.centers = null; getCenters(); }});
                mm.addEventListener('mousemove', function(e) {{ schedule(nearestIndex(e.clientY)); }});
                mm.addEventListener('mouseleave', function() {{ schedule(scrollIndex()); }});
                if (scrollEl) {{
                    scrollEl.addEventListener('scroll', function() {{
                        NS.centers = null; // chat scrolled; cached hover centers are stale
                        schedule(scrollIndex());
                    }});
                    window.addEventListener('resize', function() {{ invalidate(); }});
                }}
            }}
            // DOM may have changed (new messages): re-query and repaint from scroll position.
            invalidate();
            var init = scrollIndex();
            lastPainted = init;
            applyGradient(init);
        }}

        // Global Cmd+K / Ctrl+K for quick switcher (sidebar search button carries
        // the .nav-search-bar hook class)
        document.addEventListener("keydown", function(e) {{
            if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {{
                e.preventDefault();
                const btn = document.querySelector(".nav-search-bar");
                if (btn) btn.click();
            }}
        }});
        // Smart scroll pin: 用户主动向上滚动后解除跟随,回到底部附近才恢复
        const PIN_EPS = 48;
        const pinned = {{ value: true }};
        function markPinned(el) {{
            if (!el) return;
            pinned.value = el.scrollHeight - el.scrollTop - el.clientHeight < PIN_EPS;
        }}
        function setUpPin() {{
            const chatEl = document.getElementById("chat-scroll");
            if (chatEl && !chatEl.__pinWired) {{
                chatEl.__pinWired = true;
                chatEl.addEventListener("scroll", function() {{ markPinned(chatEl); }});
            }}
        }}
        let scrollTimeout = null;
        const observer = new MutationObserver(function(mutations) {{
            setupMinimap();
            const chatEl = document.getElementById("chat-scroll");
            if (!chatEl) return;
            let insideTools = false;
            for (let i = 0; i < mutations.length; i++) {{
                const target = mutations[i].target;
                if (target && target.closest && target.closest(".tool-accordion")) {{
                    insideTools = true;
                    break;
                }}
            }}
            if (insideTools) return;
            setUpPin();
            if (pinned.value) {{
                clearTimeout(scrollTimeout);
                scrollTimeout = setTimeout(function() {{ scrollToBottom(true); }}, 30);
            }}
        }});
        setUpPin();
        setTimeout(function() {{ scrollToBottom(false); }}, 150);
        observer.observe(document.body, {{ childList: true, subtree: true }});
        setupMinimap();

        // ---- Composer 输入键守卫（kit Composer 接 LiveView 的两处补偿）----
        //
        // 1) IME：kit Composer 的 textarea（.chat-composer-input）没有组合态判别，
        //    Enter 无条件走它的 onkeydown → on_submit；fcitx/ibus 上「回车选字」
        //    同样是 Enter，会把还没成句的拼音发出去。组合态在 Rust 侧读不到
        //    （KeyboardEvent 不带 isComposing），故在 document 捕获阶段拦：该阶段
        //    严格先于元素上的处理器，stopImmediatePropagation 让这次 Enter 到不了
        //    kit 的提交。isComposing 在 fcitx/ibus + Linux 上单靠它不可靠，故显式
        //    跟踪 compositionstart/end；keyCode 229 是旧内核兼容位。Shift+Enter
        //    （换行）不拦；组合结束后下一次单独 Enter 照常提交。
        //
        // 2) 原生 Enter 行为：Dioxus 的 Event::prevent_default 在 LiveView 下是
        //    空操作（事件走 websocket，无法阻塞浏览器默认行为），所以 kit 侧的
        //    prevent_default 拦不住 textarea 的换行插入——旧手写输入框靠这里的
        //    原生 preventDefault 兜住。同理 textarea 一旦被用户输入过，属性回写
        //    不会同步 DOM value，提交后草稿框仍留着旧文本（旧实现同样在这里
        //    清空）。两件事都在捕获阶段做，且不拦传播：提交仍由 kit 的 Rust
        //    处理器完成，这里只补浏览器侧的默认行为与视图同步。
        const composer_composing = {{ active: false }};
        function inComposerInput(el) {{
            return !!(el && el.closest && el.closest(".chat-composer-input"));
        }}
        function clearComposerInput() {{
            const el = document.querySelector(".chat-composer-input");
            if (el && el.value.trim() !== "") el.value = "";
        }}
        document.addEventListener("compositionstart", function(e) {{
            if (inComposerInput(e.target)) composer_composing.active = true;
        }}, true);
        document.addEventListener("compositionend", function(e) {{
            if (inComposerInput(e.target)) composer_composing.active = false;
        }}, true);
        document.addEventListener("keydown", function(e) {{
            if (e.key !== "Enter" || e.shiftKey) return;
            if (!inComposerInput(e.target)) return;
            if (composer_composing.active || e.isComposing || e.keyCode === 229) {{
                e.stopImmediatePropagation();
                return;
            }}
            e.preventDefault();
            clearComposerInput();
        }}, true);
        // send 圆钮（kit 的 .chat-action-round）：点击提交后同样要清掉草稿框，
        // 否则 textarea 的 DOM value 与已清空的 draft 信号不一致。
        document.addEventListener("click", function(e) {{
            const btn = e.target.closest && e.target.closest("button.chat-action-round");
            if (!btn || btn.disabled || btn.title !== "Send") return;
            setTimeout(clearComposerInput, 0);
        }}, true);

        // ---- 附件桥：file input -> base64 JSON -> 隐藏 textarea ----
        // LiveView 只认 input/change 事件，所以文件不进 form post，而是读成
        // base64 后写进 #attachment-bridge，由 Rust 侧 oninput 解析。
        // media type 一律按文件后缀推断并限白名单：伪造的 type 会进入
        // provider 的 data: URL，daemon 侧还会再校验一次。
        var ATTACHMENT_TYPES = {{
            "png": "image/png",
            "jpg": "image/jpeg",
            "jpeg": "image/jpeg",
            "gif": "image/gif",
            "webp": "image/webp",
        }};
        var ATTACHMENT_MAX_BYTES = 10 * 1024 * 1024;
        var ATTACHMENT_MAX_COUNT = 4;
        // LiveView 首帧是异步渲染：脚本加载时 composer 的 #attachment-input /
        // #attachment-bridge 可能尚未进 DOM，load 时 getElementById + addEventListener
        // 会拿到 null 而整段跳过（→ 选图永不生效）。改用 document 级 change 委托，
        // 事件触发时惰性取元素，无论 input 何时渲染都生效。
        function renderRejected(items) {{
            var rejectedEl = document.getElementById("attachment-rejected");
            if (!rejectedEl) return;
            if (items.length === 0) {{
                rejectedEl.classList.add("hidden");
                rejectedEl.innerHTML = "";
                return;
            }}
            rejectedEl.classList.remove("hidden");
            rejectedEl.innerHTML = items.map(function (it) {{
                // Names come from the file picker, so escape before
                // innerHTML — otherwise a crafted filename can inject a
                // <script> into the composer.
                var safe = String(it.name).replace(/[&<>"]/g, function (c) {{
                    return {{ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }}[c];
                }});
                return '<div style="display:flex;align-items:center;gap:6px;font-size:11px;color:#f87171;">' +
                    '<span style="overflow:hidden;text-overflow:ellipsis;max-width:180px;">' + safe + '</span>' +
                    '<span style="color:#9ca3af;">' + it.reason + '</span></div>';
            }}).join("");
        }}
        document.addEventListener("change", function (event) {{
            var pickerEl = event.target;
            if (!pickerEl || pickerEl.id !== "attachment-input") return;
            var bridgeEl = document.getElementById("attachment-bridge");
            if (!bridgeEl) return;
            var files = Array.prototype.slice.call(pickerEl.files || []);
            pickerEl.value = "";
            if (files.length === 0) return;
            var picked = [];
            var rejected = [];
            var pending = files.slice(0, ATTACHMENT_MAX_COUNT);
            var overLimit = files.length - pending.length;
            if (overLimit > 0) rejected.push({{ name: "", reason: "exceeds " + ATTACHMENT_MAX_COUNT + " attachments" }});
            var inFlight = 0;
            function refreshReading() {{
                var readingEl = document.getElementById("attachment-reading");
                if (!readingEl) return;
                if (inFlight > 0) {{
                    readingEl.classList.remove("hidden");
                    readingEl.textContent = "处理中 " + inFlight + " 项…";
                }} else {{
                    readingEl.classList.add("hidden");
                }}
            }}
            pending.forEach(function (file) {{
                var ext = (file.name.split(".").pop() || "").toLowerCase();
                var mediaType = ATTACHMENT_TYPES[ext];
                if (!mediaType) {{
                    rejected.push({{ name: file.name, reason: "unsupported type ." + ext }});
                    return;
                }}
                if (file.size > ATTACHMENT_MAX_BYTES) {{
                    rejected.push({{ name: file.name, reason: "exceeds " + (ATTACHMENT_MAX_BYTES / 1048576) + "MB" }});
                    return;
                }}
                inFlight++;
                refreshReading();
                var reader = new FileReader();
                reader.onload = function () {{
                    var result = reader.result || "";
                    var comma = result.indexOf(",");
                    picked.push({{
                        name: file.name,
                        media_type: mediaType,
                        data: comma >= 0 ? result.slice(comma + 1) : result,
                    }});
                    bridgeEl.value = JSON.stringify(picked);
                    bridgeEl.dispatchEvent(new Event("input", {{ bubbles: true }}));
                    inFlight--;
                    refreshReading();
                }};
                reader.onerror = function () {{
                    inFlight--;
                    rejected.push({{ name: file.name, reason: "read failed" }});
                    refreshReading();
                    renderRejected(rejected);
                }};
                reader.readAsDataURL(file);
            }});
            renderRejected(rejected);
        }});
    }})();
    </script>
</body>
</html>"#
    );

    let app = axum::Router::new()
        .route(
            "/ws",
            axum::routing::get(move |ws: axum::extract::WebSocketUpgrade| async move {
                ws.on_upgrade(move |socket| async move {
                    _ = view
                        .launch_virtualdom(dioxus_liveview::axum_socket(socket), move || {
                            VirtualDom::new(App)
                        })
                        .await;
                })
            }),
        )
        .fallback(axum::routing::get(move || async move {
            axum::response::Html(index_html.clone())
        }));

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {addr}: {e}");
            return;
        }
    };
    println!("kymido web server running on http://{addr}");
    let _ = axum::serve(listener, app).await;
}
