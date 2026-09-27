// Ainotation MCP 同步桥——omenic 实例（dev 工具，仅本地）。
// 与 ferrite/apps/admin-web/scripts/ainotation-bridge.mjs 同构（协议细节见该文件头），
// 默认值对齐 omenic：origin :8026（oi-web 默认端口）、桥端口 44091（避开 ferrite 的 44090）、
// 项目名 omenic-web、项目目录 = omenic 仓库根（scripts/ 位于 bin/web/scripts/，根往上两级）。
//
// 用法：先起 ainotation service（`just aino-service` 或 npx @ainotation/mcp service），
// 然后 `node bin/web/scripts/ainotation-bridge.mjs`（保持运行即持续续租 5 分钟 grant 租约）。
// 改了 ainotation-entry.ts / 升级 SDK 后重建 bundle 并重启 oi-web。
import { readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';

const SERVICE_USER_FILE = `${homedir()}/.ainotation/service/connection.json`;
const SDK_CONNECTION = new URL('../assets/ainotation/connection.json', import.meta.url);
const ORIGIN = process.env.AINO_ORIGIN ?? 'http://127.0.0.1:8026';
const FILE_PORT = Number(process.env.AINO_PORT ?? 44091);
const PROJECT_NAME = 'omenic-web';
// scripts/ 位于 <omenic-root>/bin/web/scripts/，根 = 往上两级。
const PROJECT_DIRECTORY = fileURLToPath(new URL('../../..', import.meta.url));
const RENEW_INTERVAL_MS = 2 * 60 * 1000;

async function api(url, token, path, method = 'GET', body) {
  const res = await fetch(url + path, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      ...(body ? { 'Content-Type': 'application/json' } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status} ${text.slice(0, 200)}`);
  return text ? JSON.parse(text) : null;
}

async function main() {
  let svc;
  for (let i = 0; ; i++) {
    try {
      svc = JSON.parse(await readFile(SERVICE_USER_FILE, 'utf8'));
      break;
    } catch {
      if (i > 60) throw new Error('service connection.json not available after 120s');
      await new Promise((r) => setTimeout(r, 2000));
    }
  }
  let url = svc.url;
  let token = svc.token;

  let project;
  try {
    project = await api(url, token, '/control/projects', 'POST', {
      directory: PROJECT_DIRECTORY,
      declaration: { name: PROJECT_NAME },
    });
  } catch (error) {
    const projects = await api(url, token, '/control/projects');
    project = (projects.projects ?? []).find(
      (p) => (p.projectId ?? p.config?.projectId) && (p.name ?? p.config?.name) === PROJECT_NAME,
    );
    if (!project) throw error;
  }
  const projectId = project.projectId ?? project.config.projectId;
  console.log('project ready:', projectId, project.name ?? project.config?.name);

  let grantToken;
  async function issue() {
    const grant = await api(url, token, '/control/grants', 'POST', {
      kind: 'browser',
      projectId,
      origin: ORIGIN,
    });
    grantToken = grant.token;
    await writeFile(SDK_CONNECTION, `${JSON.stringify({ url, token: grant.token }, null, 2)}\n`);
    console.log('grant issued', grant.grantId, 'expires', new Date(grant.expiresAt).toISOString());
    return grant;
  }
  let grant = await issue();

  setInterval(async () => {
    try {
      grant = await api(url, token, `/control/grants/${grant.grantId}/renew`, 'POST');
      console.log('renewed, expires', new Date(grant.expiresAt).toISOString());
    } catch (error) {
      console.error('renew failed, re-issuing:', String(error).slice(0, 160));
      // 服务可能已重启（新端口/token 写回发现文件）：先刷新再退避重发。
      // 失败不致命——留旧 grant 顶到下一次 tick 再试，桥自身不因服务抖动退出。
      try {
        const fresh = JSON.parse(await readFile(SERVICE_USER_FILE, 'utf8'));
        url = fresh.url;
        token = fresh.token;
      } catch {
        /* 发现文件缺失时沿用旧值 */
      }
      for (let i = 0; i < 15; i++) {
        try {
          grant = await issue();
          break;
        } catch {
          await new Promise((r) => setTimeout(r, 4000 * Math.min(i + 1, 5)));
        }
      }
    }
  }, RENEW_INTERVAL_MS);

  createServer((req, res) => {
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({ url, token: grantToken }));
  }).listen(FILE_PORT, '127.0.0.1', () => console.log(`connection file served on :${FILE_PORT}`));
}

main().catch((error) => {
  console.error('bridge fatal:', error);
  process.exit(1);
});
