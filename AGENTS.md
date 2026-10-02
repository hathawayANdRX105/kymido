<!-- managed by canon agents.yaml @ 2026-10-02 -->
## kymido 约定

#### Issue/PR 创建

创建 issue/PR 前必须读 `.github/ISSUE_TEMPLATE/` 或 `.github/PULL_REQUEST_TEMPLATE.md`，然后通过已安装 gate 拦截的 `gh` 创建，禁止绕过 gate。

```bash
### 安装/更新拦截门
canon init

### issue(正文按 .github/ISSUE_TEMPLATE/ 下模板)
gh issue create --title "..." --body "..." --label <epic|sub|...>

### PR(正文按 .github/PULL_REQUEST_TEMPLATE.md)
gh pr create --title "..." --body "..." --head <branch>
```

gate 自动做创建前校验(规则在 `.githooks/spec/`)+ 创建后现实校验，FAIL 拒绝创建。

**`.githooks/` 是 gate 自己的领地，agent 禁止改动 gate 规则**（`hooks/`、`spec/` 下的 gate 规则 yaml、`gate` 二进制）。gate 规则的增删改由用户或 gate 自身的 `canon init` 负责；agent 遇到 gate FAIL 应改自己的提交/PR 正文去迎合规则，而不是去改规则。UI 契约 yaml（7 份，含 `anchors`/`target` 字段）也平铺在 `.githooks/spec/`，但它们不是 gate 规则，由 `bin/web/tests/ui_contract.rs` 消费。

#### 构建与验证（CI 驱动）

**测试一律不在本地跑。** `cargo test` / `cargo clippy` / 全量 `cargo build` / `npm install` 全部交给 PR 的 CI（`.github/workflows/ci.yml`）。本地跑测试属违规操作，即使套了配额也不允许。

本地只允许这三类轻量验证：
- `cargo fmt --check`（秒级，提交前必跑——commit checklist 会拦不合格的 rust）
- `cargo check -p <crate>`（单 crate 类型检查，**不得**加 `--workspace` / `--all-targets`）
- `grep` / `ls` / `git` / 文件读写等只读命令

验证节奏：本地 `fmt --check` + 单 crate `cargo check` → push → **CI 出结果才算验证过**。CI 红了看日志改，不要在本地复现。

**唯一例外**是 web UI 需要肉眼确认时的 `cargo build --bin oi-web`（见下文启动序列），必须套 `systemd-run --user --scope -p CPUQuota=65% --`：

```bash
systemd-run --user --scope -p CPUQuota=65% -- cargo build --bin oi-web
```

#### 禁改区

`.githooks/` 归 gate 自身维护，**任何开发任务都不得改动 gate 规则**（包括 `.githooks/spec/` 下的 gate 规则 yaml、hook 脚本）。规则要改先去 demo 沙盒（见下文）验证，并由用户显式指派。例外：UI 契约 yaml（用户指定）与 gate 规则平铺在 `.githooks/spec/`，由 ui_contract.rs 消费，不算 gate 规则。

#### Demo 验证沙盒

验证 issue/PR 流程、gh-gate 拦截、规则改动时，**不要在本仓库(kymido)直接创建 demo issue/PR**，使用专用沙盒：

- 仓库：https://github.com/hathawayANdRX105/demo-githooks(本地 `~/projects/demo-githooks`)
- 用途：验证 epic/sub/PR 链路、checkbox 强制、双向关联(GT-04b)、审查强制等，避免污染 kymido
- .githooks 与 kymido 同步；规则改动后先在此仓库验证，再同步到其他项目(deskctl / new-api)

#### TUI（已删除）

终端 UI（Ratatui）已在 dsh web 复刻转正时删除：`crates/tui/` 与 `specs/tui/` 均不存在，别再去找。前端验证全部走上面的 Web（oi-web）与「Web UI 契约验收」两节。

`.agent/skills/ui-validation/SKILL.md` 仍保留，但其描述的 TestBackend/specs/tui 流程已无对应代码，用到时先核实目标是否存在。

#### Web（oi-web）启动与样式缺失排查

**症状**：Web UI 打开后是「裸文本」——没有暗色主题、没有卡片/气泡样式，文字堆在一起（如 "搜索会话⌘K / 工作区 / Spaces" 全是素文本），但 JS 功能正常（能发消息、minimap 逻辑在跑）。这是**二进制里嵌进去的 CSS 为空**，不是前端没渲染、也不是没合并代码。

**根因（三个坑，按发生顺序）**：

1. `bin/web/build.rs` 需要 `bin/web/node_modules` 里的 `@tailwindcss/cli`，把 `assets/tailwind-input.css` 生成 `tailwind.gen.css`（写到 `OUT_DIR`），再由 `bin/web/src/app.rs` 的 `include_str!` 嵌进二进制。**没装依赖时 npx 回退拉不到 `tailwindcss` 包本体（输入里 `@import "tailwindcss"` 解析失败），build.rs 会静默写一个 0 字节 CSS 兜底**（见 build.rs 注释 "UI would simply be unstyled"）。`node_modules` 不在 git 里，CI / 新克隆 / 新 worktree 都没装 → 全是空 CSS。
2. `cargo build` 只把 `assets/tailwind-input.css` 和 `src` 标了 `rerun-if-changed`，**装了 `node_modules` 之后它感知不到，不会自动重跑 build.rs**，旧的空 CSS 会一直沿用。必须 `touch bin/web/assets/tailwind-input.css` 再编译才重新生成。
3. `cargo run` 起的是**编译前那一刻的二进制**；编译完成后产物已换，但进程还在跑旧的（进程启动时间比二进制 mtime 还早）。必须杀掉重启。

**正确启动序列**（在仓库根目录）：

```bash
cd bin/web && npm install                # 确保 node_modules/.bin/tailwindcss 存在
### 可选：手动确认能生成非空 CSS（约 40KB）
###   node_modules/.bin/tailwindcss -i .tailwind.gen-input.css -o /tmp/t.css
cd <仓库根>
touch bin/web/assets/tailwind-input.css   # 强制重跑 build.rs
cargo build --bin oi-web                  # 不是 -p web；二进制在 bin/web（oi-web）
pkill -x oi-web; sleep 1
nohup ./target/debug/oi-web > /tmp/oi-web.log 2>&1 &      # 起在默认 8026（PORT 可覆盖）
### 验样式：页面内联 <style> 块的字节数（2026-09-16 实测 40005，含 .flex/.mx-auto/padding-left）
curl -s localhost:8026/ | python3 -c 'import sys,re;h=sys.stdin.read();m=re.search(r"<style>(.*?)</style>",h,re.S);print("style bytes:",len(m.group(1)) if m else "NONE")'
```

**判样式不要用 `--color-accent` 计数**：dsh 复刻自研组件落地后该 token 只剩 minimap JS 别名在用，正常二进制里就 4 处，低计数不代表空 CSS。可靠判据是上面伺服页 `<style>` 块的字节数（非 0 且含真实工具类即正常）。若要直接检查生成的 CSS，按**属性值**（如 `padding-left`）而非选择器 grep——Tailwind v4 输出未压缩，选择器里的 `.` / `[]` 会被转义，按选择器 grep 容易漏判。

**快速诊断**：`<style>` 块字节数为 0 或 NONE = 跑的是空 CSS 的旧二进制，按上面序列重建重启。浏览器记得**硬刷新**（LiveView 按 origin 缓存 wasm，换端口/换二进制后旧缓存不失效）。

**CI / 协作**：`node_modules` 未进 git，CI 与任何新克隆/新 worktree 都要先 `npm install` 再 build，否则 UI 无样式。若要让 build 可复现，考虑把 `node_modules` 入库或让 build.rs 失败时报错而非静默写空文件。

#### Web UI 契约验收（.githooks/spec 下 7 份 UI 契约）

web UI（dsh 设计语言复刻，C5.1 已验收）的视觉/结构锁在 `.githooks/spec/` 下的 7 份 UI 契约 yaml（workspace / chat / sidebar / stats / settings / quick-switcher / taskpanel），防止后续接线（5.2a/5.2b）破坏。改动 web 组件样式或布局时：先跑契约测试，再按下表浏览器抽查。

**契约测试**：`bin/web/tests/ui_contract.rs`，**由 CI 跑，本地不跑**（见上文「构建与验证」）。本地只做静态核对：改了组件 class 就同步改 `.githooks/spec/` 下对应 UI 契约 yaml 的 `find` 锚点，用 grep 确认锚点字符串在实现文件里真实存在。

**yaml 字段约定**：`name`（契约名）/ `target`（kymido 实现文件，相对仓库根）/ `description` / `anchors`（锚点列表，每项 `key` + `find`（源码中稳定 class 片段或静态字面量）+ `expect`（预期形态）+ `source`（kymido 实现位置 + dsh 出处）+ 可选 `file`（锚点级实现文件覆盖，默认用 target））/ `notes`。测试两类断言：① 每个 yaml 可被 serde_yaml 解析且字段齐全；② 每个 `find` 关键字在对应实现文件中出现。新增 spec 必须同步登记 `tests/ui_contract.rs` 的 `SPEC_FILES`。

**起服**：按上文「正确启动序列」起 oi-web（记得 npm install + touch css + 重建重启，浏览器硬刷新）。

**浏览器抽查点**（每屏挑核心）：

- `workspace.yaml`——三列 grid：侧栏 280px 拖拽夹取 264–420、折叠后 rail 56px；中栏只有 44px 面包屑头（无顶栏）；分支 chip 品牌蓝。
- `chat.yaml`——发送消息后「工作过程」折叠行出现（24px 头 + 计数）；用户气泡右对齐 r22、max-w 525；发送钮 34px 圆形品牌蓝；状态行 12px 居中（model · tokens · $cost · context）。
- `sidebar.yaml`——logo 行 52px + 18px 字标；新会话钮 h38 r12；项目行 34 / 会话行 32（缩进 22px，状态点 brand/dim/danger）；时间戳 hover 隐藏；底部数据统计/设置行 42px。
- `stats.yaml`——KPI 卡一行 5 张；指标带一行 7 格；主体三列 320/1fr/340；吞吐折线品牌蓝。
- `settings.yaml`——弹窗 800px r24、左导航 188px（单元 h40 r12）；「关于」页有版本号 chip。
- `quick-switcher.yaml`——⌘K/Ctrl+K 弹出 560px 顶部对齐面板；输入 h44、会话行 h40；ESC 退出。
- `taskpanel.yaml`——composer 上方 dock 卡宽随消息列（≤780）；进度条 1px 品牌蓝；filter chip h26 r7；任务卡 r10。

**已知偏差（记录不改）**：dsh 消息列 748px，kymido 消息列与 composer 统一 `max-w-[780px]`（chat.yaml notes）。

## 发现处置纪律

自动检查（canon 的 `FAIL`/`WARN`、`jev` L3 语义发现、CRG / `ocr review` 审查意见）
产出的是**发现**，不是判决。每条发现都必须被显式处置，不存在"绕过"这个选项。

### 先读规范，再改代码

1. 拿到 finding，先读规则原文，确认这条发现到底要求什么：
   - canon 规则总览：`gate-spec` skill（正本）；各仓 `.githooks/spec/docs/SPEC_OVERVIEW.md` 为播种副本
   - 单条规则的参数（匹配范围 / 严重度 / harness）：`.githooks/spec/**/<rule>.yaml`
   - 项目适配说明（本仓为什么这么定）：`.agent/rules/gates.md`
2. 不确定 finding 是否成立时，读完规则仍不能判定 → **记为待裁决**并在交付记录里写明，
   不要凭猜测改代码，也不要直接忽略。

### 按根因修，不按症状修

- finding 指向的**约束**是根因。修代码使约束成立，而不是让检查不再报。
- 修完自问：这条约束在本仓还成立吗？下次同类改动还会不会触发？

### 完整读输出，不截断

- 拦截信息**逐条读完**再动手。`| head -5`、`| tail`、`grep -v` 会吞掉后面的 finding，
  让人误以为已经修完。
- 报告里出现「N checks passed」时，确认 N 覆盖了你改动的部分。

### 禁止糊弄式修复

以下动作一律视为违规（无论 canon 是否因此变绿）：

| 禁止 | 为什么 | 正确做法 |
|---|---|---|
| 改 `.githooks/spec/` 规则、降低 `fail_severity`、删 spec 文件 | 把约束改没，不是修问题 | 开 issue 说明规则缺陷，交维护者决定 |
| `--no-verify`、跳过钩子、直接推 | 绕过的是整个门禁体系 | 修到清零；规则有误走 issue |
| `head` / `tail` / `grep -v` 截断输出后当没看见 | 后面的 finding 被吞 | 完整读输出 |
| 加 `#[allow(dead_code)]` / `# noqa` 消告警 | 压制信号而非解决 | 删无用代码，或写清保留理由 |
| 建空文件 / 空目录 / 占位文件骗过目录类规则 | 结构噪音 | 真按规则合并或删除 |
| 给无断言测试塞 `assert!(true)` | 测试变成永真装饰 | 断言真实行为；无行为可测就删测试 |
| 拆分 / 改名 / 移动只为躲过匹配范围 | 破坏结构换绿灯 | 按规则设计的结构改 |

### 逐条处置并留下书面说明

- **每条 finding 一个处置**：修复（默认）或**书面驳回**。
- 修复 → 在交付记录里写：`规则 ID → 根因 → 改法（file:line）`。
- 驳回 → 必须写 `规则 ID + 不修理由 + 依据`，由维护者裁决。沉默即违规。
- 交付记录落点：PR 正文 `## Delivery record` 段，或 issue 的交付评论。
- WARN 与 FAIL 同等对待。WARN 只是不拦，不是可忽略。

### 规范层级

- `.githooks/` 是 canon 领地：agent 不改规则。
- `.agent/rules/`、`specs/rules/` 是规范正本：发现规则与现实冲突 → 提 issue，不自行改写。
- 本纪律与各仓既有条款冲突时，以本纪律为准（它更严格）。

## 代码风格

### 命名与结构

- 函数名动宾结构、见名知目的（`parse_channel_config` 而不是 `do_config`）。
- 公共 API 写文档注释（用途、参数、错误、示例），模块头写 `//!`。
- 变量与类型不缩写到看不出含义；短名只留给公认短物（`id`、`ctx`、`err`）。

### 注释

- 注释写**为什么**，不复述代码在做什么。
- 不留 AI 味注释（`// Step 1:` / `// This function` / `// 该函数…` / `// 首先…然后…`）。
- 需要解释的复杂逻辑，宁可提取成命名清晰的函数，也不要靠注释块描述流程。
- 注释掉的代码直接删；git 记得它。

### 占位符与未完成

- 未实现的函数或 trait 用语言原生宏，并带 issue 号：
  - Rust：`todo!("TODO(#123): 说明这里要做什么")` / `unimplemented!("…")`
- TODO / FIXME 注释必须带 issue 号：`// TODO(#123): …`。
- 不留空的 `todo!()` / `pass` / `NotImplemented` 桩而无说明。

### 复用与删除

- 动手前先找同仓同类实现与已装依赖。已有工具能解决就不新写。
- 新增依赖前确认：标准库能做完？已装依赖能做？确实都需要才加。
- **删除优于新增**：不留兼容垫片、旧别名、废弃分支、注释掉的旧实现。
- 改了接口就同步迁移所有调用方，不留双路径兼容。

### 工具

- 命名、缩进、格式化交给项目工具（`cargo fmt` / `gofmt` / `ruff format` / `prettier` / `biome`），
  不手工对齐，不在格式化工具之外争论风格。
- lint 报错逐条判断：真问题就修；误报就在规则允许的方式下局部豁免并写明理由，
  不整文件关掉。

## Rust 开发性能

本仓 `.cargo/config.toml` 已配 `jobs = 4`（多会话并发上限）与
`rustc-wrapper = sccache`（跨 worktree 编译缓存），`Cargo.toml` 已关增量、
降 debuginfo。配置随 cargo 向上搜索对 `.wt/*` worktree 自动生效。

- 跑测试用 `just test-fast`：testless 函数级影响分析，只跑本次改动可能破坏的
  测试；testless 异常/零命中自动降级全量，绝不静默跳过。全量务必
  `cargo test --workspace`（根包 workspace 下裸 `cargo test` 只跑根包）。
- 不要在会话里自行 `export RUSTC_WRAPPER` 或改 jobs——统一走仓配置；
  重命令照旧套 cgroup CPU 配额（`systemd-run --user --scope -p CPUQuota=70% --`）。
- 增量编译已关（缓存优先）：同树连续小改动按 crate 级重编是预期行为，不是
  回归；若本仓热重载明显变慢，提 issue 议局部放开。
- 新建 `.wt` worktree 直接用；旧布局 worktree 若报 workspace 收编错误，
  根因与修法见 canon 仓 `Cargo.toml` 的 `exclude` 注释。
- 配置细节、坑清单与实测基线：skill `rust-dev-perf`。

## 构建与验证

### 基线

- 改动前先确认基线状态。基线已经红就先说清，别把自己的问题和既有问题混在一起报。

### 验证行为，不是验证代码存在

- 改完跑**真实命令**验证："跑一下" = 启动实际程序、调用实际接口、发真实请求、观察输出或状态。
- bug 修复先复现再修，修完确认复现路径不再触发。
- 永久性改动要留一个能抓住真实回归的检查。
- 测可观察行为与边界：状态迁移、转换、优先级、真实错误、边界值。
  不测 plumbing、不断言源码文本、不写永真断言、不测 mock 的回声。
- 测试与被测文件就近放 `tests/`（同名对应），保持全量套件可通过。

### 重命令放对位置

- 全量测试、全量构建、全量 lint 放 CI 或收尾阶段，不在改动过程中反复跑。
- 本地只跑轻量、快的针对性检查（单 crate `cargo check`、单包测试、`fmt --check`、
  类型检查）。
- 需要本地跑重命令时，套 cgroup CPU 配额（`systemd-run --user --scope -p CPUQuota=70% --`
  或本仓等价手段），不抢占用户正在用的 CPU——与「Rust 开发性能」章节同值，
  两处不要各写一个数。
- 装依赖、打包等命令同样受限。

### 收尾

- 一次跑完该跑的检查（测试 + lint + 类型），不在半成品状态下宣称通过。
- 验证不了的部分（缺运行环境、缺凭据、缺硬件）明确说"未验证 + 为什么"，
  不把"没跑"说成"通过"。
- 不因为失败就改测试迎合实现。测试红了先判断是实现错还是测试错。

## 破坏性操作与敏感信息

### 删除

- 删文件前确认它确实是废弃物（生成物、已合并的临时文件），不是"看起来没用"。
- 用可恢复的方式删（`gio trash`），不用不可恢复的直接删除。
- `rm -rf`、覆盖写、清空数据库这类不可逆操作：**先说明影响，等确认**。
- 删的是别人的产物、你不理解用途的文件、或 gitignore 里的东西 → 停下来问。

### 敏感与不可逆

- 凭据、token、密钥、私钥：不打印到输出、不写进提交、不粘到 issue/PR 正文。
- 不擅自 dump 整个配置文件或环境变量（可能含密钥）。要看就只看需要的字段。
- 系统级配置、字体、全局环境、dotfiles 里的全局项：默认别动，改动前先问。
- 数据库迁移、配置格式变更、依赖大版本升级：先确认可回滚。

### 安装与全局改动

- 装包、改 PATH、装 systemd 服务、改 shell 配置：先确认再动。
- 写进 dotbot / 配置管理器托管范围的路径前，先确认该由谁管。
- 不可逆的系统级改动（分区、引导、网络栈）一律先问，不自行执行。

## 调查与审查

### 先建图，再查调用

- 调查陌生代码先建调用图谱（`code-review-graph update`）再查调用关系，
  不逐文件翻、不靠 grep 猜。
- 改共享逻辑前先看影响面（谁在调、调了会怎样），再动。

### 审查两层

1. **结构层**：`code-review-graph detect-changes` 看结构影响、循环依赖、风险面。
2. **规范层**：`ocr review`（**代码审查工具**，与 OCR 截图识别无关）看代码规范。
   按模块分批喂，不要一次性喂整个仓。

- 审查发现逐条处置：修或书面驳回（同发现处置纪律）。
- 改完核心逻辑后跑一次工具审查再收工。

### 写代码的模型 ≠ 审查的模型

- 自己写的代码自己审有盲区。审查方尽量指定与写作不同的模型。
- 无法可靠判断"这段是谁写的" → 开工前问一句，别猜。

### UI 验证

- 交互元素加 `data-testid`（值取稳定标识，如 `name` 属性），容器加 `role` + `aria-label`。
- 冒烟验证用结构化快照（`tab.ariaSnapshot()`）做 role / name / testid 断言。
- 禁区：只靠截图肉眼判断、用 class 选择器断言、绕过结构化快照直接提 PR。
- 截图能证明"看起来对"，不能证明"结构对、可访问、可自动化"。

### 别造 demo 污染真实仓

- 验证 issue/PR 流程、gh 拦截、规则改动，用专用沙盒仓（如 `demo-githooks`），
  不在业务仓创建 demo issue/PR。

## 提交与 PR

### 分支

- 默认分支是 `main`（本仓若不同以本仓为准），功能从默认分支拉。
- 一个任务一个分支，分支名带类型前缀（`feat/` / `fix/` / `refactor/` / `chore/`）。
- 合并后清理已合并分支与 worktree，不留 stale 分支。

### Commit

- 标题走 conventional commit（`feat:` / `fix:` / `refactor:` / `docs:` / `chore:` /
  `test:` / `ci:` / `build:` / `perf:` / `style:` / `revert:`）。
- 标题**用英文**，正文可用中文。
- 一个 commit 一件事。不把无关改动、格式化噪声、生成物混进逻辑改动。
- 提交前跑对应检查（`canon pre-commit` / `canon pre-push`），不靠推送失败才发现。

### Issue

- 标题中文；正文 heading 英文、内容中文。
- sub-issue 必须自包含：正文不写 `Parent:` / `Related:` / PR 占位符，直接写清它要什么。
- 关闭前 `Done when` 的 checkbox 全勾。

### PR

- 标题纯英文（conventional commit 风格）；正文小节标题英文、内容中文。
- 正文按仓库模板（`.github/PULL_REQUEST_TEMPLATE.md`）写：背景 / 改了什么 / 为什么 /
  实现步骤 / 交付记录 / 怎么验证 / 检查清单。
- 关联 issue 用 `Fixes #<n>` 收尾行；draft 阶段用 `Related #<n>`，合并授权前改 `Fixes`。
- 开启或更新 PR 后看 CI 结果到底（`gh pr checks`），红了就修，不等用户来问。
- 被 canon 拦下就修代码，**不改规则**。规则确有缺陷 → 开 issue 交维护者裁决。

### 收尾

- 收尾时清掉：已合并分支、临时 worktree、临时进程、跑完的 dev server。
- 资源及时释放；只保留维护者需要的进程（如用户要看的 web 前端）。

## 子代理与并行

- 派子代理时 prompt 写全：目标文件 / 符号、要做什么、验收标准、明确不做什么。
- 子代理之间不共享未写进文件的结论；结论落文件，不落聊天。
- 独立切片才并行；有依赖的按序做。
- 子代理声称改完的东西要自己核对，不以"它说完成了"为凭据。
- 大范围改动派只读代理先摸清结构，再动手改。
