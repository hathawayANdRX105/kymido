// Ainotation 开发标注工具入口（仅开发环境，由 app.rs 的 debug_assertions 门控注入）。
// 打包：npm run aino → assets/ainotation/ainotation.iife.js
//
// MCP 接线：从本地 kymido 同步桥（scripts/ainotation-bridge.mjs，127.0.0.1:44091）
// 取 {url, token}（grant token 由桥签发并续租）；桥不在时降级为纯本地模式
// （标注/复制/导出可用，MCP 不自动同步）。
import { createAinotation } from '@ainotation/sdk';

const PROJECT_ID = 'kymido-web';

function mount(options) {
  const inspector = createAinotation({ projectId: PROJECT_ID, ...options });
  void inspector.mount();
}

function start(mcp) {
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => mount(mcp), { once: true });
  } else {
    mount(mcp);
  }
}

async function loadConnection() {
  try {
    const res = await fetch('http://127.0.0.1:44091/connection.json', { cache: 'no-store' });
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

loadConnection()
  .then((cfg) => start(cfg ? { mcp: { endpoint: cfg.url, token: cfg.token } } : {}))
  .catch(() => start({}));
