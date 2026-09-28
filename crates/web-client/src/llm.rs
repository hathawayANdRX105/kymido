//! Real LLM integration and configuration persistence for kymido web.

use serde::{Deserialize, Serialize};
use std::path::Path;
use web_state::types::ChatMessage;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRuntimeConfig {
    /// active 路由解析出的 base_url（无 providers 文件或解析失败时为空）。
    pub base_url: String,
    /// active 路由解析出的 api_key（`api_key_env` 优先于内联 `api_key`）。
    pub api_key: String,
    /// active 路由解析出的 model（provider 行 `default_model` 或路由命名 model）。
    pub model: String,
    /// active 路由的 max_tokens（model 行 > provider 行 > 页面默认）。
    pub max_tokens: u32,
    pub data_dir: String,
    /// active 路由所属 provider 名（"provider/model" 的 provider 部分）；
    /// 保存时写回 `providers.toml` 的 `[providers.<active_provider>]` 行。
    pub active_provider: String,
    /// active 路由原文（`providers.toml` 根 `active`）；空串 = 未配置。
    pub active: String,
    /// 设置页「MCP 服务器」表单的行数据。空 vec 表示表单没有服务器，
    /// 保存时完全不碰 `[mcp]`（不创建、不改写）。
    pub mcp_servers: Vec<McpServerForm>,
    /// active provider 的 fallback 路由（providers 行 `fallbacks`，顺序即瀑布序）。
    /// 保存时整体写回该行的 `fallbacks` 键（删行 = 删条目）。
    pub fallback_routes: Vec<String>,
    /// active model 声明的 context window（token）；0 = 未声明（引擎默认预算，
    /// 展示层用 web-state 的 DEFAULT_CONTEXT_MAX 兜底）。
    pub context_max: u32,
    /// active model 是否声明 image 输入（T5 附件门）；未声明 = false（附件关闭）。
    pub image_input: bool,
}

/// 设置页单个 MCP 服务器的表单行（web 侧 DTO，不依赖 config crate）：
/// 与 `crates/infra/config` 的 `McpServerConfig` 一一对应，但统一成文本框
/// 友好的 `String`——空串表示「未设置」，`args` 是逗号分隔文本。
///
/// `env` / `reconnect` 不进表单（高级键，直接编辑 TOML）：保存路径按
/// `name` 匹配已有服务器，这两个键（及任何未知键）逐字保留。
///
/// `Default` 是「添加服务器」按钮的空白行：全空串 + false。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServerForm {
    /// 短句柄；保存时按它匹配 `[[mcp.servers]]` 里的既有表。
    pub name: String,
    /// 可执行文件；空串 = 未设置（HTTP 服务器改用 `url`）。
    pub command: String,
    /// streamable-HTTP 端点；空串 = 未设置。
    pub url: String,
    /// 参数列表的逗号分隔文本（UI 形态，保存时切回 `Vec<String>`）。
    /// 局限：参数本身含逗号无法在此文本框表达。
    pub args: String,
    /// 子进程工作目录；空串 = 未设置（继承进程 cwd）。
    pub cwd: String,
    /// 单次 tool call 超时毫秒数的十进制文本；空串 = 用 crate 默认。
    pub tool_call_timeout_ms: String,
    /// 启动失败是否中止整个 MCP bring-up（false 与 serde 默认等价）。
    pub fail_on_startup_error: bool,
}

impl Default for LlmRuntimeConfig {
    fn default() -> Self {
        Self::load_from_system()
    }
}

impl LlmRuntimeConfig {
    /// Load settings: `config.toml` for the root keys (`data_dir` / `model`)
    /// and `[[mcp.servers]]`; `providers.toml` (same directory) for
    /// credentials and capability data — the flat `[llm]` section no longer
    /// exists (loading one is a config-crate hard error).
    pub fn load_from_system() -> Self {
        let mut model = String::new();
        let mut data_dir = "./.kymido".to_string();
        let mut mcp_servers = Vec::new();

        // 1. config.toml candidates (same shadowing as the daemon).
        for path in [
            "./.kymido/config.toml",
            "../.kymido/config.toml",
            "kymido.toml",
        ] {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(value) = content.parse::<toml::Value>() {
                    if let Some(d) = value.get("data_dir").and_then(|v: &toml::Value| v.as_str()) {
                        data_dir = d.to_string();
                    }
                    if let Some(m) = value.get("model").and_then(|v: &toml::Value| v.as_str()) {
                        model = m.to_string();
                    }

                    // [mcp] → 表单行。没有该段（或 servers 不是表数组）时
                    // 保持空 vec，表单显示「未配置」。
                    if let Some(servers) = value
                        .get("mcp")
                        .and_then(|m| m.get("servers"))
                        .and_then(toml::Value::as_array)
                    {
                        for server in servers {
                            if let Some(form) = mcp_server_form_from_value(server) {
                                mcp_servers.push(form);
                            }
                        }
                    }
                }
                break;
            }
        }

        // 2. providers.toml next to the config file (sibling-file contract).
        let mut base_url = String::new();
        let mut api_key = String::new();
        let mut max_tokens = 4096u32;
        let mut active_provider = String::new();
        let mut active = String::new();
        let mut fallback_routes = Vec::new();
        let mut context_max = 0u32;
        let mut image_input = false;
        for path in [
            Path::new("./.kymido").join("providers.toml"),
            Path::new("../.kymido").join("providers.toml"),
            Path::new(&data_dir).join("providers.toml"),
        ] {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            match toml::from_str::<config::ProvidersFile>(&content) {
                Ok(file) => {
                    if let Some(route_text) = file.active.as_deref() {
                        active = route_text.to_string();
                        if let Some(route) = file.resolve_route(route_text) {
                            if let Some((provider, _)) =
                                config::ProvidersFile::split_route(route_text)
                            {
                                active_provider = provider;
                            }
                            let image = route.image_input();
                            base_url = route.base_url;
                            api_key = route.api_key;
                            model = route.model;
                            max_tokens = route.max_tokens.unwrap_or(4096);
                            context_max = route.context_window.unwrap_or(0);
                            image_input = image;
                            if let Some(entry) = file.provider(&active_provider) {
                                fallback_routes = entry.fallbacks.clone();
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("warn: providers.toml ({}) 解析失败: {e}", path.display());
                }
            }
            break;
        }

        Self {
            base_url,
            api_key,
            model,
            max_tokens,
            data_dir,
            active_provider,
            active,
            mcp_servers,
            fallback_routes,
            context_max,
            image_input,
        }
    }

    /// Persist the root keys + `[[mcp.servers]]` to `.kymido/config.toml`.
    ///
    /// **增量写回**：若文件已存在，用 [`toml_edit::DocumentMut`] 只更新本结构体
    /// 管理的键——根表的 `omp_path` / `data_dir` / `model` 与按 `name` 逐台编辑的
    /// `[[mcp.servers]]`（见 [`Self::mcp_servers`]）——**其余内容原样保留**：
    /// `[memory]` / `[daemon]` 等未管理段、MCP 服务器上的 `env` / `reconnect` /
    /// 未知键、注释、空行与排版。凭据与能力数据**不在此文件**（`[llm]` 段已随
    /// 切换移除，写回它会让 daemon 的 `Config::load` 直接报错）——走
    /// [`Self::save_providers_to_file`]。
    pub fn save_to_file(&self) -> Result<(), String> {
        let dir = Path::new(&self.data_dir);
        if !dir.exists() {
            let _ = std::fs::create_dir_all(dir);
        }

        let target_path = dir.join("config.toml");

        // 文件已存在：增量更新，未管理区域逐字节保留。
        if let Ok(content) = std::fs::read_to_string(&target_path) {
            match content.parse::<toml_edit::DocumentMut>() {
                Ok(mut doc) => {
                    self.write_managed_keys(&mut doc)?;
                    return std::fs::write(&target_path, doc.to_string()).map_err(|e| {
                        format!("写入配置文件 {} 失败: {}", target_path.display(), e)
                    });
                }
                // 解析失败时绝不能让用户配置凭空消失：先备份原文，再回退全量写。
                Err(e) => {
                    let backup_path = target_path.with_extension("toml.bak");
                    if let Err(backup_err) = std::fs::write(&backup_path, &content) {
                        return Err(format!(
                            "配置文件 {} 解析失败({})，且备份原始内容到 {} 也失败({})；已中止写入以避免丢失配置",
                            target_path.display(),
                            e,
                            backup_path.display(),
                            backup_err
                        ));
                    }
                    eprintln!(
                        "warn: 配置文件 {} 解析失败({})，已备份为 {} 后回退全量写",
                        target_path.display(),
                        e,
                        backup_path.display()
                    );
                    return self
                        .write_full_config(&target_path, preserve_omp_path(&content).as_deref());
                }
            }
        }

        self.write_full_config(&target_path, None)
    }

    /// Persist the providers side to `.kymido/providers.toml`（与
    /// `save_to_file` 的两文件分工，见设计文档 T6）。
    ///
    /// 管理键：根 `active` 与 `[providers.<active_provider>]` 行下的
    /// `base_url` / `api_key` / `default_model` / `max_tokens` / `fallbacks`
    /// （`api_key_env`、`models` 表与未知键逐字保留）。`active_provider` 为
    /// 空（首次配置、providers 文件尚不存在）时用稳定名 `"default"` 建行。
    pub fn save_providers_to_file(&self) -> Result<(), String> {
        // 什么可编辑内容都没有时不建文件（避免空壳 providers.toml）。
        if self.active_provider.is_empty()
            && self.base_url.trim().is_empty()
            && self.model.trim().is_empty()
        {
            return Ok(());
        }
        let dir = Path::new(&self.data_dir);
        if !dir.exists() {
            let _ = std::fs::create_dir_all(dir);
        }
        let target_path = dir.join("providers.toml");

        if let Ok(content) = std::fs::read_to_string(&target_path) {
            match content.parse::<toml_edit::DocumentMut>() {
                Ok(mut doc) => {
                    self.write_providers_managed_keys(doc.as_table_mut())?;
                    return std::fs::write(&target_path, doc.to_string()).map_err(|e| {
                        format!("写入 providers 文件 {} 失败: {}", target_path.display(), e)
                    });
                }
                Err(e) => {
                    // providers 文件解析失败：备份后全量写（与 config.toml 同语义）。
                    let backup_path = target_path.with_extension("toml.bak");
                    if let Err(backup_err) = std::fs::write(&backup_path, &content) {
                        return Err(format!(
                            "providers 文件 {} 解析失败({})，且备份到 {} 也失败({})；已中止写入",
                            target_path.display(),
                            e,
                            backup_path.display(),
                            backup_err
                        ));
                    }
                    eprintln!(
                        "warn: providers 文件 {} 解析失败({})，已备份为 {} 后回退全量写",
                        target_path.display(),
                        e,
                        backup_path.display()
                    );
                    return self.write_full_providers(&target_path);
                }
            }
        }

        self.write_full_providers(&target_path)
    }

    /// 全量写一份只含管理键的 `providers.toml`（文件不存在或不可解析时使用）。
    fn write_full_providers(&self, target_path: &Path) -> Result<(), String> {
        let provider = if self.active_provider.is_empty() {
            "default".to_string()
        } else {
            self.active_provider.clone()
        };
        let active = if self.active.trim().is_empty() {
            provider.clone()
        } else {
            self.active.clone()
        };
        let fallbacks: Vec<String> = self
            .fallback_routes
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let content = format!(
            "# kymido providers（与 config.toml 同目录的兄弟文件；凭据唯一来源）\n\
             active = \"{}\"\n\n\
             [providers.{provider}]\n\
             base_url = \"{}\"\n",
            active,
            self.base_url.trim()
        );
        let mut content = if self.api_key.trim().is_empty() {
            content
        } else {
            format!("{content}api_key = \"{}\"\n", self.api_key.trim())
        };
        content.push_str(&format!(
            "default_model = \"{}\"\nmax_tokens = {}\n",
            self.model.trim(),
            self.max_tokens
        ));
        if !fallbacks.is_empty() {
            let items = fallbacks
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ");
            content.push_str(&format!("fallbacks = [{items}]\n"));
        }
        std::fs::write(target_path, content)
            .map_err(|e| format!("写入 providers 文件 {} 失败: {}", target_path.display(), e))
    }

    /// 把 providers 管理键写进已解析的文档；其它键、注释、排版一律不动。
    fn write_providers_managed_keys(&self, root: &mut toml_edit::Table) -> Result<(), String> {
        let provider = if self.active_provider.is_empty() {
            "default".to_string()
        } else {
            self.active_provider.clone()
        };
        // 根 `active`：缺失才补（不抹掉用户手工路由）；值为 provider 名
        // （走 default_model 解析）或既有路由原文。
        if !root.contains_key("active") {
            let active = if self.active.trim().is_empty() {
                provider.clone()
            } else {
                self.active.trim().to_string()
            };
            root.insert("active", toml_edit::value(active));
        }

        let base_url = self.base_url.trim();
        if base_url.is_empty() {
            return Err("base_url 为空，已中止保存（providers 行必须可发起请求）".to_string());
        }
        if self.model.trim().is_empty() {
            return Err(
                "model 为空，已中止保存（default_model 是 provider 行的硬要求）".to_string(),
            );
        }

        let providers = ensure_sub_table(root, "providers")?;
        let entry = ensure_sub_table(providers, &provider)?;
        set_item(entry, "base_url", toml_edit::value(base_url));
        set_or_clear_str(entry, "api_key", &self.api_key);
        set_item(entry, "default_model", toml_edit::value(self.model.trim()));
        // TOML 整数是 i64；u32 → i64 无损。
        set_item(
            entry,
            "max_tokens",
            toml_edit::value(self.max_tokens as i64),
        );

        let fallbacks: Vec<String> = self
            .fallback_routes
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if fallbacks.is_empty() {
            entry.remove("fallbacks");
        } else {
            let mut arr = toml_edit::Array::new();
            for f in &fallbacks {
                arr.push(f.as_str());
            }
            set_item(entry, "fallbacks", toml_edit::value(arr));
        }
        Ok(())
    }

    /// 全量写一份只含管理键的新配置（文件不存在或原文件不可解析时使用）。
    /// `omp_path` 由调用方决定：`None` 用默认 `"omp"`，`Some(v)` 沿用原值。
    /// 空表单不写 `[mcp]` 段（与增量路径一致）；非空时复用
    /// `write_mcp_servers` 落地。
    fn write_full_config(&self, target_path: &Path, omp_path: Option<&str>) -> Result<(), String> {
        let toml_content = format!(
            "# kymido configuration\n\
             omp_path = \"{}\"\n\
             data_dir = \"{}\"\n\
             model = \"{}\"\n",
            omp_path.unwrap_or("omp"),
            self.data_dir,
            self.model
        );

        if self.mcp_servers.is_empty() {
            return std::fs::write(target_path, toml_content)
                .map_err(|e| format!("写入配置文件 {} 失败: {}", target_path.display(), e));
        }
        let mut doc = toml_content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("生成的配置文本无法解析: {}", e))?;
        write_mcp_servers(doc.as_table_mut(), &self.mcp_servers)?;
        std::fs::write(target_path, doc.to_string())
            .map_err(|e| format!("写入配置文件 {} 失败: {}", target_path.display(), e))
    }

    /// 把本结构体管理的键写进已解析的 config.toml 文档；其它键、段、注释、
    /// 排版一律不动。凭据/能力数据在 providers.toml（[`Self::save_providers_to_file`]）。
    fn write_managed_keys(&self, doc: &mut toml_edit::DocumentMut) -> Result<(), String> {
        let root = doc.as_table_mut();
        // `omp_path` 没有对应字段，只在缺失时补上默认值——直接写死 "omp" 会抹掉
        // 用户在别处配好的自定义路径，正是本修复要消除的那类静默覆盖。
        if !root.contains_key("omp_path") {
            root.insert("omp_path", toml_edit::value("omp"));
        }
        set_item(root, "data_dir", toml_edit::value(self.data_dir.as_str()));
        set_item(root, "model", toml_edit::value(self.model.as_str()));
        write_mcp_servers(root, &self.mcp_servers)
    }

    /// Test connection by querying /v1/models
    pub fn test_connection(&self) -> Result<Vec<String>, String> {
        let clean_base = self.base_url.trim_end_matches('/');
        let url = if clean_base.ends_with("/v1") {
            format!("{}/models", clean_base)
        } else {
            format!("{}/v1/models", clean_base)
        };

        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .timeout(std::time::Duration::from_secs(8))
            .call()
            .map_err(|e| format!("连接失败: {}", e))?;

        let json: serde_json::Value = resp
            .into_json()
            .map_err(|e| format!("解析返回 JSON 失败: {}", e))?;

        let mut models = Vec::new();
        if let Some(list) = json.get("data").and_then(|v| v.as_array()) {
            for item in list {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    models.push(id.to_string());
                }
            }
        }

        if models.is_empty() {
            models.push(self.model.clone());
        }

        Ok(models)
    }

    /// Call real chat completions endpoint
    pub fn chat(&self, messages: &[ChatMessage]) -> Result<String, String> {
        let clean_base = self.base_url.trim_end_matches('/');
        let url = if clean_base.ends_with("/v1") {
            format!("{}/chat/completions", clean_base)
        } else {
            format!("{}/v1/chat/completions", clean_base)
        };

        // Convert messages to openai format
        let payload_messages: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content
                })
            })
            .collect();

        let body = serde_json::json!({
            "model": self.model,
            "messages": payload_messages,
            "max_tokens": self.max_tokens,
            "temperature": 0.7
        });

        let resp = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(45))
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    let text = resp.into_string().unwrap_or_default();
                    format!("API 状态码错误 {}: {}", code, text)
                }
                ureq::Error::Transport(t) => format!("网络传输错误: {}", t),
            })?;

        let json: serde_json::Value = resp
            .into_json()
            .map_err(|e| format!("解析回复 JSON 失败: {}", e))?;

        if let Some(content) = json["choices"][0]["message"]["content"].as_str() {
            Ok(content.to_string())
        } else {
            Err("回复中未包含有效的 message.content".to_string())
        }
    }
}

/// 写入一个管理键：已存在就原地替换 value，不存在才 insert。
///
/// 不能无条件用 `Table::insert`——它对已存在的键会调 `Key::fmt()`，把 key 的
/// decor 清掉，而 toml_edit 把「行前注释」（如 `# provider endpoint`）挂在
/// key 的 decor 上，那样注释就跟着没了。只换 value 时 key 与其 decor 原地不动，
/// 注释/排版得以保留。
fn set_item(table: &mut toml_edit::Table, key: &str, item: toml_edit::Item) {
    if let Some(slot) = table.get_mut(key) {
        *slot = item;
    } else {
        table.insert(key, item);
    }
}

/// 确保 `parent.child` 是表并返回其可变引用；缺失就创建，存在但不是表就报错
///（与 `[mcp]` 的畸形段防御同语义）。
fn ensure_sub_table<'a>(
    parent: &'a mut toml_edit::Table,
    child: &str,
) -> Result<&'a mut toml_edit::Table, String> {
    if !parent.contains_key(child) {
        parent.insert(child, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    parent
        .get_mut(child)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| format!("[{child}] 段已存在但不是表，无法增量更新"))
}

/// 把表单的 MCP 服务器行写进文档的 `[[mcp.servers]]` 表数组（G8-B 语义的
/// MCP 延伸）：
///
/// - 空表单**完全不碰** `[mcp]`——不创建该段，已有的服务器、注释、排版
///   原样保留（与没有 MCP 表单的旧版保存行为一致）；
/// - 表单里的每一行按 `name` 匹配既有表：命中就只替换管理键
///   （command/url/args/cwd/tool_call_timeout_ms/fail_on_startup_error），
///   `env` / `reconnect` / 未知键及其注释逐字保留；未命中才追加新表；
/// - 文件里有、表单里没有的服务器一律不动——表单是追加/按名编辑，
///   绝不整体重写。
fn write_mcp_servers(root: &mut toml_edit::Table, forms: &[McpServerForm]) -> Result<(), String> {
    if forms.is_empty() {
        return Ok(());
    }

    if !root.contains_key("mcp") {
        root.insert("mcp", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    // 与 [llm] 同样的畸形防御：段存在但不是表时返回错误而不是 panic。
    let mcp = root
        .get_mut("mcp")
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| "[mcp] 段已存在但不是表，无法增量更新".to_string())?;
    if !mcp.contains_key("servers") {
        mcp.insert(
            "servers",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let servers = mcp
        .get_mut("servers")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
        .ok_or_else(|| {
            "[mcp].servers 已存在但不是 [[mcp.servers]] 表数组，无法增量更新".to_string()
        })?;

    for form in forms {
        let name = form.name.trim();
        if name.is_empty() {
            // name 是匹配键：没有它既无法定位旧表也无法命名新表，
            // 与其静默追加一台无名服务器，不如让调用方修好表单再存。
            return Err("[mcp] 存在缺少 name 的服务器，已中止保存".to_string());
        }
        let existing = servers
            .iter()
            .position(|t| t.get("name").and_then(toml_edit::Item::as_str) == Some(name));
        match existing {
            Some(idx) => {
                // 匹配键本身不重写（保住 name 上的注释与排版），只动管理键。
                let table = servers.get_mut(idx).expect("position 刚返回的索引必然存在");
                update_server_managed_keys(table, form)?;
            }
            None => {
                let mut table = toml_edit::Table::new();
                table.insert("name", toml_edit::value(name));
                update_server_managed_keys(&mut table, form)?;
                servers.push(table);
            }
        }
    }
    Ok(())
}

/// 把一个服务器表单行的管理键写进单个 `[[mcp.servers]]` 表。
///
/// 空串字段表示「未设置」：原地**清键**而不是写空串/零值（与 config crate
/// 的 `Option` / `#[serde(default)]` 语义一致，也避免留下 `command = ""`
/// 这类会让 spawn 失败的值）；`fail_on_startup_error` 为 false 时同样清键
/// （false 就是 serde 默认）。`env` / `reconnect` / 未知键根本不触碰。
fn update_server_managed_keys(
    table: &mut toml_edit::Table,
    form: &McpServerForm,
) -> Result<(), String> {
    set_or_clear_str(table, "command", &form.command);
    set_or_clear_str(table, "url", &form.url);

    let args = split_args_text(&form.args);
    if args.is_empty() {
        table.remove("args");
    } else {
        let mut arr = toml_edit::Array::new();
        for arg in args {
            arr.push(arg);
        }
        set_item(table, "args", toml_edit::value(arr));
    }

    set_or_clear_str(table, "cwd", &form.cwd);

    let timeout_text = form.tool_call_timeout_ms.trim();
    if timeout_text.is_empty() {
        table.remove("tool_call_timeout_ms");
    } else {
        let ms: u64 = timeout_text.parse().map_err(|_| {
            format!(
                "[mcp] 服务器 {:?} 的 tool_call_timeout_ms 不是有效的毫秒数: {:?}",
                form.name.trim(),
                timeout_text
            )
        })?;
        let ms = i64::try_from(ms).map_err(|_| {
            format!(
                "[mcp] 服务器 {:?} 的 tool_call_timeout_ms 超出可表示范围",
                form.name.trim()
            )
        })?;
        set_item(table, "tool_call_timeout_ms", toml_edit::value(ms));
    }

    if form.fail_on_startup_error {
        set_item(table, "fail_on_startup_error", toml_edit::value(true));
    } else {
        table.remove("fail_on_startup_error");
    }
    Ok(())
}

/// 写一个「空串 = 未设置」的字符串管理键：非空就原地替换 value（保住键的
/// 注释与排版），空串则把整个键清掉。
fn set_or_clear_str(table: &mut toml_edit::Table, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        table.remove(key);
    } else {
        set_item(table, key, toml_edit::value(value));
    }
}

/// 逗号分隔文本 → 参数列表：按逗号切分、逐项去首尾空白、丢弃空项。
/// 局限见 [`McpServerForm::args`]：参数本身含逗号时直接编辑 TOML。
fn split_args_text(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 把一个 `[[mcp.servers]]` 的 toml 值转成表单行；缺 `name` 的条目返回
/// `None`（不进表单）。无名条目因此也永远不会被保存路径改写——保存按
/// `name` 匹配，表单外的一切原样保留。
fn mcp_server_form_from_value(server: &toml::Value) -> Option<McpServerForm> {
    let name = server.get("name")?.as_str()?;
    let args = server
        .get("args")
        .and_then(toml::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    Some(McpServerForm {
        name: name.to_string(),
        command: toml_str_field(server, "command"),
        url: toml_str_field(server, "url"),
        args,
        cwd: toml_str_field(server, "cwd"),
        tool_call_timeout_ms: server
            .get("tool_call_timeout_ms")
            .and_then(toml::Value::as_integer)
            .map(|n| n.to_string())
            .unwrap_or_default(),
        fail_on_startup_error: server
            .get("fail_on_startup_error")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
    })
}

/// 取一个字符串字段的值，缺失或类型不符时给空串（表单语义：未设置）。
fn toml_str_field(server: &toml::Value, key: &str) -> String {
    server
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 从一份**不可解析**的原始配置里抢救 `omp_path` 的值（若有）。
///
/// 文件能正常解析时走增量路径，`write_managed_keys` 已经「缺失才补」地保住了
/// 自定义路径；只有解析失败回退全量写时才需要这里——用行扫描而非 toml 解析，
/// 因为调用前提就是这个文件解析不了。格式必须是 `omp_path = "value"`。
fn preserve_omp_path(unparsed: &str) -> Option<String> {
    for line in unparsed.lines() {
        let trimmed = line.trim();
        // 必须 continue 而不是 ?：真实配置首行通常是注释，`?` 会让第一行
        // 不匹配就把整个函数返回成 None，第 2 行的 omp_path 于是被丢弃——
        // 正是这个函数要避免的丢失。
        let rest = match trimmed.strip_prefix("omp_path") {
            Some(r) => r,
            None => continue,
        };
        let rest = rest.trim_start();
        let rest = match rest.strip_prefix('=') {
            Some(r) => r,
            None => continue,
        };
        let rest = rest.trim();
        let value = rest.strip_prefix('"').and_then(|r| r.strip_suffix('"'));
        if let Some(v) = value {
            return Some(v.to_string());
        }
        // 无引号形式也接受，取到行尾（去注释与空白）。
        return Some(rest.split('#').next().unwrap_or(rest).trim().to_string());
    }
    None
}
