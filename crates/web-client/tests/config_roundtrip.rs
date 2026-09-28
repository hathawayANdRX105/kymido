//! `LlmRuntimeConfig` 的双文件往返测试：`config.toml`（根键 + `[[mcp.servers]]`）
//! 与 `providers.toml`（active 路由 + provider 行 + 瀑布路由）各管一摊——
//! `[llm]` 扁平段已随切换移除（config crate 加载即硬错误），凭据唯一来源
//! 是 providers 文件。
//!
//! 覆盖：`save_to_file` / `save_providers_to_file` → `load_from_system` 的
//! 往返、providers 侧能力数据（context window / input 模态）读侧、`[mcp]`
//! 段按 name 的增量编辑（env/reconnect/未知键/注释保留），以及两份文件都
//! 不存在时的兜底默认值。
//!
//! **为什么全部串行**：`load_from_system` 读的是相对路径
//! （`./.kymido/...` 等），cargo 默认多线程跑测试，若并行改 CWD 会互相
//! 打架，所以这里用一把全局锁把它们排成队。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use web_client::llm::{LlmRuntimeConfig, McpServerForm};

fn serial_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 进入一个隔离的临时工作目录。
///
/// 返回的 guard 在 drop 时恢复原 CWD；`_lock` 保证同一时刻只有一个用例在
/// 动这些全局状态（前一个用例 panic 导致的锁中毒不该连累后续用例，故
/// 主动 `into_inner`）。
///
/// **CWD 刻意再下沉一层**（`<tempdir>/work`）：`load_from_system` 的候选
/// 路径里有 `../.kymido/config.toml`，若直接站在 tempdir 根上，`..` 会落到
/// 共享的系统临时目录（`/tmp`），别人留下的 `.kymido/config.toml` 就会被读进
/// 来，用例随机变红。下沉一层后 `./` 与 `../` 都还在本用例的 tempdir 内。
struct Sandbox {
    _lock: MutexGuard<'static, ()>,
    original_cwd: PathBuf,
    work: PathBuf,
    _dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let lock = serial_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_cwd = std::env::current_dir().expect("读取当前目录失败");
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).expect("创建工作子目录失败");
        std::env::set_current_dir(&work).expect("切换到临时目录失败");
        Sandbox {
            _lock: lock,
            original_cwd,
            work,
            _dir: dir,
        }
    }

    /// 当前工作目录（相对路径都以此为基准）。
    fn path(&self) -> &Path {
        &self.work
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original_cwd);
    }
}

/// 装配一个 `LlmRuntimeConfig` 字面量（测试用；未关心的字段给零值/空表单——
/// mcp_servers / fallback_routes 需要时用返回值的字段直接赋值）。
fn make_cfg(
    base_url: &str,
    api_key: &str,
    model: &str,
    max_tokens: u32,
    active_provider: &str,
    active: &str,
) -> LlmRuntimeConfig {
    LlmRuntimeConfig {
        base_url: base_url.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
        max_tokens,
        data_dir: "./.kymido".to_string(),
        active_provider: active_provider.to_string(),
        active: active.to_string(),
        mcp_servers: Vec::new(),
        fallback_routes: Vec::new(),
        context_max: 0,
        image_input: false,
    }
}

/// 两份文件的完整往返：providers 侧管理键（base_url/api_key/model/
/// max_tokens/active/fallbacks）逐一读回，MCP 表单随行保存。
#[test]
fn save_then_load_roundtrips_managed_fields() {
    let sb = Sandbox::new();

    let mut saved = make_cfg(
        "http://127.0.0.1:9999/v1",
        "sk-roundtrip-fixture",
        "agnes-3.0-pro",
        8192,
        "local",
        "local/agnes-3.0-pro",
    );
    saved.fallback_routes = vec!["other/backup-model".to_string()];
    saved.save_to_file().expect("保存 config 失败");
    saved.save_providers_to_file().expect("保存 providers 失败");
    assert!(
        sb.path().join(".kymido/config.toml").is_file(),
        "save_to_file 应在 data_dir 下写出 config.toml"
    );
    assert!(
        sb.path().join(".kymido/providers.toml").is_file(),
        "save_providers_to_file 应在 data_dir 下写出 providers.toml"
    );

    let loaded = LlmRuntimeConfig::load_from_system();
    assert_eq!(loaded.base_url, saved.base_url);
    assert_eq!(loaded.api_key, saved.api_key);
    assert_eq!(loaded.model, saved.model);
    assert_eq!(loaded.max_tokens, saved.max_tokens);
    assert_eq!(loaded.active_provider, saved.active_provider);
    assert_eq!(loaded.active, saved.active);
    assert_eq!(loaded.fallback_routes, saved.fallback_routes);
    assert_eq!(loaded.mcp_servers, saved.mcp_servers);
}

/// 只有 config.toml（没有 providers 文件）时：凭据字段为空，model 回落根表
/// model，能力字段为零值——不得编造数据。
#[test]
fn load_without_providers_yields_empty_credentials() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "data_dir = \"./.kymido\"\nmodel = \"top-level-model\"\n",
    )
    .expect("写配置失败");

    let loaded = LlmRuntimeConfig::load_from_system();
    assert_eq!(loaded.base_url, "");
    assert_eq!(loaded.api_key, "");
    assert_eq!(loaded.model, "top-level-model", "根表 model 应生效");
    assert_eq!(loaded.max_tokens, 4096);
    assert_eq!(loaded.context_max, 0);
    assert!(!loaded.image_input);
    assert_eq!(loaded.active_provider, "");
    assert_eq!(loaded.active, "");
}

/// 两份文件都不存在时 `load_from_system` 不 panic，返回兜底默认值。
#[test]
fn load_without_any_config_file_yields_defaults() {
    let _sb = Sandbox::new();

    let loaded = LlmRuntimeConfig::load_from_system();
    assert_eq!(loaded.data_dir, "./.kymido");
    assert_eq!(loaded.model, "");
    assert_eq!(loaded.base_url, "");
    assert_eq!(loaded.max_tokens, 4096);
    // `Default` 就是 `load_from_system`，两者必须一致
    assert_eq!(LlmRuntimeConfig::default(), loaded);
}

/// 往返是幂等的：读回来的配置再存一次，两份文件内容都逐字节相同。
#[test]
fn resaving_a_loaded_config_is_byte_identical() {
    let sb = Sandbox::new();

    let original = make_cfg(
        "https://relay.example.com",
        "sk-idempotent",
        "agnes-2.5-flash",
        4096,
        "local",
        "local/agnes-2.5-flash",
    );
    original.save_to_file().expect("首次保存 config 失败");
    original
        .save_providers_to_file()
        .expect("首次保存 providers 失败");

    let cfg_path = sb.path().join(".kymido/config.toml");
    let prv_path = sb.path().join(".kymido/providers.toml");
    let first_cfg = std::fs::read_to_string(&cfg_path).expect("读取 config 失败");
    let first_prv = std::fs::read_to_string(&prv_path).expect("读取 providers 失败");

    let loaded = LlmRuntimeConfig::load_from_system();
    loaded.save_to_file().expect("二次保存 config 失败");
    loaded
        .save_providers_to_file()
        .expect("二次保存 providers 失败");

    let second_cfg = std::fs::read_to_string(&cfg_path).expect("读取 config 失败");
    let second_prv = std::fs::read_to_string(&prv_path).expect("读取 providers 失败");
    assert_eq!(first_cfg, second_cfg, "config load → save 不得改变文件内容");
    assert_eq!(
        first_prv, second_prv,
        "providers load → save 不得改变文件内容"
    );
}

/// `save_to_file` / `save_providers_to_file` 会按需创建 data_dir（配置页
/// 首次保存时 `.kymido` 往往不存在）。
#[test]
fn save_creates_a_missing_data_dir() {
    let sb = Sandbox::new();

    let nested = sb.path().join("deep/nested/.kymido");
    assert!(!nested.exists(), "前置条件：目标目录尚不存在");

    let mut cfg = make_cfg(
        "http://localhost:3182",
        "sk-mkdir",
        "m",
        128,
        "local",
        "local/m",
    );
    // 用例要点：data_dir 指向一个尚不存在的深层路径，save_to_file 得把
    // 整条目录链建出来。make_cfg 默认是 "./.kymido"（扁平、CWD 相对），
    // 这里改成相对 CWD 的嵌套路径，nested 即其落盘位置。
    cfg.data_dir = "deep/nested/.kymido".to_string();
    cfg.save_to_file()
        .expect("保存 config 到不存在的目录应自动建目录");
    assert!(nested.join("config.toml").is_file());
}

/// providers 读侧（T4/T5）：`[[providers.<p>.models]]` 的 context_window 进
/// `context_max`，`input` 含 "image" 时 `image_input` 为 true；未声明 model
/// 条目时两字段都为零值（不编造能力数据）。
#[test]
fn providers_load_reads_capability_data() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/providers.toml"),
        "active = \"local/pro-vision\"\n\n\
         [providers.local]\n\
         base_url = \"http://cap.example\"\n\
         api_key = \"sk-cap\"\n\
         default_model = \"pro\"\n\
         fallbacks = [\"other/backup\"]\n\n\
         [[providers.local.models]]\n\
         id = \"pro\"\n\
         context_window = 65536\n\n\
         [[providers.local.models]]\n\
         id = \"pro-vision\"\n\
         context_window = 32768\n\
         input = [\"text\", \"image\"]\n",
    )
    .expect("写 providers 失败");

    let loaded = LlmRuntimeConfig::load_from_system();
    assert_eq!(loaded.model, "pro-vision");
    assert_eq!(
        loaded.context_max, 32768,
        "active model 条目的 context_window"
    );
    assert!(loaded.image_input, "input 含 image → 附件门放开");
    assert_eq!(loaded.fallback_routes, vec!["other/backup".to_string()]);

    // 未声明能力条目的 model（pro）：换 active 后两字段回落零值。
    std::fs::write(
        sb.path().join(".kymido/providers.toml"),
        "active = \"local/pro\"\n\n\
         [providers.local]\n\
         base_url = \"http://cap.example\"\n\
         api_key = \"sk-cap\"\n\
         default_model = \"pro\"\n\n\
         [[providers.local.models]]\n\
         id = \"pro\"\n",
    )
    .expect("写 providers 失败");
    let loaded = LlmRuntimeConfig::load_from_system();
    assert_eq!(loaded.context_max, 0, "未声明 context_window → 零值");
    assert!(!loaded.image_input, "未声明 input → 缺省 text-only");
}

/// 核心回归（T6）：`save_providers_to_file` 只更新 active provider 的管理键，
/// 其它 provider 行、models 能力表、注释一律逐字保留（toml_edit 嵌套表数组
/// 的专门回归）。
#[test]
fn providers_save_preserves_unmanaged_entries() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/providers.toml"),
        "# kymido providers\n\
         active = \"local/m\"\n\
         \n\
         [providers.local]\n\
         base_url = \"http://old\"\n\
         api_key = \"sk-old\"\n\
         default_model = \"m\"\n\
         \n\
         # 第二个 provider：用户手工维护，表单不碰\n\
         [providers.other]\n\
         base_url = \"http://other\"\n\
         api_key_env = \"OTHER_API_KEY\"\n\
         default_model = \"o\"\n\
         \n\
         [[providers.other.models]]\n\
         id = \"o\"\n\
         context_window = 128000\n\
         input = [\"text\", \"image\"]\n",
    )
    .expect("写 providers 失败");

    let mut cfg = make_cfg("http://new", "sk-new", "m", 2048, "local", "local/m");
    cfg.fallback_routes = vec!["other/o".to_string()];
    cfg.save_providers_to_file().expect("保存 providers 失败");

    let saved =
        std::fs::read_to_string(sb.path().join(".kymido/providers.toml")).expect("读回失败");
    let doc = saved
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后应为合法 TOML");

    // 管理键被更新（active provider 行）
    assert_eq!(
        doc["providers"]["local"]["base_url"].as_str(),
        Some("http://new")
    );
    assert_eq!(
        doc["providers"]["local"]["api_key"].as_str(),
        Some("sk-new")
    );
    assert_eq!(
        doc["providers"]["local"]["default_model"].as_str(),
        Some("m")
    );
    assert_eq!(
        doc["providers"]["local"]["max_tokens"].as_integer(),
        Some(2048)
    );
    let fb = doc["providers"]["local"]["fallbacks"]
        .as_array()
        .expect("fallbacks 应写出字符串数组");
    assert_eq!(fb.len(), 1);
    assert_eq!(fb.get(0).and_then(|v| v.as_str()), Some("other/o"));

    // 未管理的 provider 行逐键保留
    assert_eq!(
        doc["providers"]["other"]["base_url"].as_str(),
        Some("http://other")
    );
    assert_eq!(
        doc["providers"]["other"]["api_key_env"].as_str(),
        Some("OTHER_API_KEY")
    );
    let models = doc["providers"]["other"]["models"]
        .as_array_of_tables()
        .expect("models 表数组应保留");
    assert_eq!(models.len(), 1);
    assert_eq!(
        models.get(0).expect("models 首项应存在")["id"].as_str(),
        Some("o")
    );

    // 注释逐字保留（用户注释挂在 [providers.other] 的表 decor 上）
    assert!(
        saved.contains("# 第二个 provider：用户手工维护，表单不碰"),
        "未管理段注释应保留: {saved}"
    );
}

/// providers 文件首次保存（不存在 → 全量写）：active + active provider 行
/// 的管理键齐全，缺省 active（active_provider 空）用稳定名 "default" 建行。
#[test]
fn providers_full_write_when_missing() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");

    let cfg = make_cfg(
        "http://fresh.example",
        "sk-fresh",
        "fresh-model",
        1024,
        "",
        "",
    );
    cfg.save_providers_to_file()
        .expect("首次保存 providers 失败");

    let doc = std::fs::read_to_string(sb.path().join(".kymido/providers.toml"))
        .expect("读回失败")
        .parse::<toml_edit::DocumentMut>()
        .expect("应为合法 TOML");
    assert_eq!(
        doc["active"].as_str(),
        Some("default"),
        "首次配置 active 缺省指向 default 行"
    );
    assert_eq!(
        doc["providers"]["default"]["base_url"].as_str(),
        Some("http://fresh.example")
    );
    assert_eq!(
        doc["providers"]["default"]["api_key"].as_str(),
        Some("sk-fresh")
    );
    assert_eq!(
        doc["providers"]["default"]["default_model"].as_str(),
        Some("fresh-model")
    );
}

/// providers 增量保存遇畸形文件（providers 键不是表）→ 备份后报错，不 panic。
#[test]
fn providers_save_errors_on_malformed_file() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    // providers 是字符串而非表 → toml_edit 能解析但 ensure_sub_table 报错。
    std::fs::write(
        sb.path().join(".kymido/providers.toml"),
        "active = \"local/m\"\nproviders = \"not-a-table\"\n",
    )
    .expect("写 providers 失败");

    let res = make_cfg("http://x", "sk", "m", 100, "local", "local/m").save_providers_to_file();
    let err = res.expect_err("providers 不是表时应返回错误");
    assert!(
        err.contains("providers") || err.contains("不是表"),
        "错误信息应指出 providers 段的问题: {err}"
    );
}

/// providers 保存时 base_url 为空 → 中止（provider 行必须可发起请求；
/// 写一个空 base_url 会让 daemon 的 active 路由解析失败）。
#[test]
fn providers_save_refuses_empty_base_url() {
    let sb = Sandbox::new();
    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/providers.toml"),
        "active = \"local/m\"\n[providers.local]\nbase_url = \"http://old\"\napi_key = \"sk\"\ndefault_model = \"m\"\n",
    )
    .expect("写 providers 失败");

    let res = make_cfg("", "sk", "m", 100, "local", "local/m").save_providers_to_file();
    let err = res.expect_err("空 base_url 应中止保存");
    assert!(err.contains("base_url"), "错误应指出 base_url: {err}");
}

/// 核心回归：`save_to_file` 只更新自己管理的根键 + `[[mcp.servers]]`，
/// `[memory]` / `[daemon]` 等未管理段必须原样保留，且**不得**写出 `[llm]`
/// （daemon 加载 `[llm]` 是硬错误，写回它等于给用户埋雷）。
#[test]
fn save_preserves_unmanaged_sections_and_writes_no_llm() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "# kymido configuration\n\
         omp_path = \"omp\"\n\
         data_dir = \"./.kymido\"\n\
         model = \"agnes-2.5-flash\"\n\
         \n\
         [memory]\n\
         enabled = true\n\
         dir = \"./.kymido/memory\"\n\
         \n\
         [daemon]\n\
         cwd = \"/workspace\"\n\
         max_turns = 32\n\
         \n\
         [mcp]\n\
         \n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         command = \"npx\"\n\
         args = [\"-y\", \"@modelcontextprotocol/server-filesystem\"]\n",
    )
    .expect("写配置失败");

    make_cfg(
        "http://127.0.0.1:9999/v1",
        "sk-updated",
        "agnes-3.0-pro",
        8192,
        "local",
        "local/agnes-3.0-pro",
    )
    .save_to_file()
    .expect("保存配置失败");

    let doc = std::fs::read_to_string(sb.path().join(".kymido/config.toml"))
        .expect("读回配置失败")
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后的配置必须是合法 TOML");

    // 根表管理键被更新
    assert_eq!(doc["data_dir"].as_str(), Some("./.kymido"));
    assert_eq!(doc["model"].as_str(), Some("agnes-3.0-pro"));

    // 保存路径不得写 [llm] 段（daemon 加载即硬错误）
    assert!(
        doc.get("llm").is_none(),
        "save_to_file 不得写 [llm] 段: {doc}"
    );

    // 未管理段逐键保留
    assert_eq!(doc["memory"]["enabled"].as_bool(), Some(true));
    assert_eq!(doc["memory"]["dir"].as_str(), Some("./.kymido/memory"));
    assert_eq!(doc["daemon"]["cwd"].as_str(), Some("/workspace"));
    assert_eq!(doc["daemon"]["max_turns"].as_integer(), Some(32));

    // [[mcp.servers]] 是表数组，不是内联数组
    let servers = doc["mcp"]["servers"]
        .as_array_of_tables()
        .expect("[mcp].servers 应保留为表数组");
    assert_eq!(servers.len(), 1, "[[mcp.servers]] 条目数应保留");
    let server = servers.get(0).expect("表数组首项应存在");
    assert_eq!(server["name"].as_str(), Some("fs"));
    assert_eq!(server["command"].as_str(), Some("npx"));
    assert_eq!(
        server["args"].as_array().map(|a| a.len()),
        Some(2),
        "args 数组内容应保留"
    );
}

/// 注释由 toml_edit 原样保留（根表注释与段内注释都在）。
#[test]
fn save_preserves_comments() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "# my note\n\
         data_dir = \"./.kymido\"\n\
         model = \"m\"\n\
         \n\
         # mcp block\n\
         [mcp]\n",
    )
    .expect("写配置失败");

    make_cfg("http://x", "sk", "m", 100, "local", "local/m")
        .save_to_file()
        .expect("保存配置失败");

    let saved =
        std::fs::read_to_string(sb.path().join(".kymido/config.toml")).expect("读回配置失败");
    assert!(saved.contains("# my note"), "根表注释应保留: {saved}");
    assert!(saved.contains("# mcp block"), "段内注释应保留: {saved}");
}

/// 解析失败回退全量写时，`omp_path` 若不在第一行也必须被抢救回来。
///
/// 真实配置几乎都以注释开头，而 `preserve_omp_path` 曾在行循环里用 `?`：
/// 第一行不匹配就从整个函数返回 None，第二行的 `omp_path` 于是被丢弃，
/// 回退写把用户的自定义路径静默换成默认 "omp"。
#[test]
fn fallback_preserves_omp_path_not_on_first_line() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    // 重复的根键 model 使整份文档无法解析，强制走备份 + 全量写回退路径。
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "# kymido configuration\n\
         omp_path = \"custom-omp\"\n\
         data_dir = \"./.kymido\"\n\
         model = \"m\"\n\
         model = \"duplicate-key-breaks-parsing\"\n",
    )
    .expect("写配置失败");

    make_cfg("http://x", "sk", "m", 100, "", "")
        .save_to_file()
        .expect("保存配置失败");

    let saved =
        std::fs::read_to_string(sb.path().join(".kymido/config.toml")).expect("读回配置失败");
    assert!(
        saved.contains("custom-omp"),
        "回退全量写必须抢救第二行的自定义 omp_path: {saved}"
    );
    // 原文已备份，解析失败不丢配置。
    assert!(
        sb.path().join(".kymido/config.toml.bak").exists(),
        "解析失败时应备份原文"
    );
}

/// MCP 表单按 name 编辑既有服务器：改 `command` 后保存，`env` / `reconnect`
/// 与两处注释必须逐字保留（G8-B「只动管理键」语义延伸到 [[mcp.servers]]）。
#[test]
fn mcp_edit_preserves_env_reconnect_and_comments() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "data_dir = \"./.kymido\"\n\
         model = \"m\"\n\
         \n\
         [mcp]\n\
         \n\
         # filesystem server, spawn per session\n\
         [[mcp.servers]]\n\
         name = \"fs\"\n\
         # npx downloads on first run\n\
         command = \"npx\"\n\
         args = [\"-y\", \"@modelcontextprotocol/server-filesystem\"]\n\
         env = { RUST_LOG = \"debug\", FS_ROOT = \"/tmp\" }\n\
         \n\
         [mcp.servers.reconnect]\n\
         initial_delay_ms = 500\n\
         max_delay_ms = 30000\n\
         max_attempts = 10\n",
    )
    .expect("写配置失败");

    let mut loaded = LlmRuntimeConfig::load_from_system();

    assert_eq!(loaded.mcp_servers.len(), 1, "应加载出 1 台 MCP 服务器");
    assert_eq!(loaded.mcp_servers[0].name, "fs");
    assert_eq!(
        loaded.mcp_servers[0].args, "-y, @modelcontextprotocol/server-filesystem",
        "args 应以逗号分隔文本进表单"
    );

    // 模拟用户在表单里改 command 后保存
    loaded.mcp_servers[0].command = "npx-dlx".to_string();
    loaded.save_to_file().expect("保存配置失败");

    let saved =
        std::fs::read_to_string(sb.path().join(".kymido/config.toml")).expect("读回配置失败");
    let doc = saved
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后的配置必须是合法 TOML");

    let server = doc["mcp"]["servers"]
        .as_array_of_tables()
        .and_then(|a| a.get(0))
        .expect("[[mcp.servers]] 首项应存在");
    // 管理键确实被更新
    assert_eq!(
        server["command"].as_str(),
        Some("npx-dlx"),
        "command 应被更新"
    );
    assert_eq!(
        server["args"].as_array().map(|a| a.len()),
        Some(2),
        "args 数组应保留"
    );

    // 未管理键逐字保留
    let env = server["env"].as_inline_table().expect("env 应保留为内联表");
    assert_eq!(env.get("RUST_LOG").and_then(|v| v.as_str()), Some("debug"));
    assert_eq!(env.get("FS_ROOT").and_then(|v| v.as_str()), Some("/tmp"));
    let reconnect = server["reconnect"]
        .as_table()
        .expect("reconnect 子表应保留");
    assert_eq!(reconnect["initial_delay_ms"].as_integer(), Some(500));
    assert_eq!(reconnect["max_delay_ms"].as_integer(), Some(30000));
    assert_eq!(reconnect["max_attempts"].as_integer(), Some(10));

    // 注释逐字保留：段头注释（表 decor）与键前注释（key decor，set_item 不动它）
    assert!(
        saved.contains("# filesystem server, spawn per session"),
        "[[mcp.servers]] 头部注释应保留: {saved}"
    );
    assert!(
        saved.contains("# npx downloads on first run"),
        "managed 键前的行注释应保留: {saved}"
    );
}

/// 文件原本没有 `[mcp]`：空表单保存不创建该段；表单添加服务器后保存，
/// `[[mcp.servers]]` 才出现，且未填的键（command/args/cwd）不写出。
#[test]
fn mcp_section_written_only_when_form_has_servers() {
    let sb = Sandbox::new();

    let base = make_cfg("http://127.0.0.1:3182", "sk-b", "m", 128, "", "");
    base.save_to_file().expect("首次保存失败");

    let cfg_path = sb.path().join(".kymido/config.toml");
    let after_empty = std::fs::read_to_string(&cfg_path).expect("读回配置失败");
    assert!(
        !after_empty.contains("[mcp]"),
        "空表单不得写 [mcp] 段: {after_empty}"
    );

    let mut with_server = LlmRuntimeConfig::load_from_system();
    with_server.mcp_servers.push(McpServerForm {
        name: "fetch".to_string(),
        command: String::new(),
        url: "http://127.0.0.1:9100/mcp".to_string(),
        args: String::new(),
        cwd: String::new(),
        tool_call_timeout_ms: "1500".to_string(),
        fail_on_startup_error: true,
    });
    with_server.save_to_file().expect("带服务器保存失败");

    let doc = std::fs::read_to_string(&cfg_path)
        .expect("读回配置失败")
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后的配置必须是合法 TOML");

    let servers = doc["mcp"]["servers"]
        .as_array_of_tables()
        .expect("应写出 [[mcp.servers]] 表数组");
    assert_eq!(servers.len(), 1);
    let server = servers.get(0).expect("表数组首项应存在");
    assert_eq!(server["name"].as_str(), Some("fetch"));
    // url 型服务器：command 未填 → 不写该键（而不是留一个空串值）
    assert!(server.get("command").is_none(), "空 command 不应写出");
    assert_eq!(server["url"].as_str(), Some("http://127.0.0.1:9100/mcp"));
    assert_eq!(server["tool_call_timeout_ms"].as_integer(), Some(1500));
    assert_eq!(server["fail_on_startup_error"].as_bool(), Some(true));
    assert!(server.get("args").is_none(), "空 args 不应写出");
    assert!(server.get("cwd").is_none(), "空 cwd 不应写出");
}

/// 首次保存（文件本不存在 → 走全量写）也必须带上 `[[mcp.servers]]`。
///
/// 全量写原先只拼受管根键就落盘，而 `mcp_servers` 非空时增量路径才会写表：
/// 于是新机器上的第一次保存会静默丢掉整张服务器表，直到第二次保存（文件已
/// 存在、走增量）才重新出现。
#[test]
fn full_write_persists_mcp_servers_on_first_save() {
    let sb = Sandbox::new();

    let mut cfg = make_cfg("http://127.0.0.1:3183", "sk-c", "m", 128, "", "");
    cfg.mcp_servers = vec![McpServerForm {
        name: "fetch".to_string(),
        command: String::new(),
        url: "http://127.0.0.1:9100/mcp".to_string(),
        args: String::new(),
        cwd: String::new(),
        tool_call_timeout_ms: "1500".to_string(),
        fail_on_startup_error: true,
    }];
    // 目标文件不存在 → write_full_config 分支。
    assert!(
        !sb.path().join(".kymido/config.toml").exists(),
        "本用例的前提是配置文件尚不存在"
    );
    cfg.save_to_file().expect("首次保存失败");

    let saved =
        std::fs::read_to_string(sb.path().join(".kymido/config.toml")).expect("读回配置失败");
    let doc = saved
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后的配置必须是合法 TOML");
    let servers = doc["mcp"]["servers"]
        .as_array_of_tables()
        .expect("全量写也必须落地 [[mcp.servers]]");
    assert_eq!(servers.len(), 1, "服务器不得在首次保存时丢失");
    let server = servers.get(0).expect("表数组首项应存在");
    assert_eq!(server["name"].as_str(), Some("fetch"));
    assert_eq!(server["url"].as_str(), Some("http://127.0.0.1:9100/mcp"));
    assert_eq!(server["tool_call_timeout_ms"].as_integer(), Some(1500));
}

/// 表单状态里没有的服务器保存后原样保留——包括其 `env` 与未知键：
/// 表单是「按 name 追加/编辑」，绝不整体重写 `[mcp]`（删卡不等于删配置）。
#[test]
fn mcp_servers_missing_from_form_are_left_untouched() {
    let sb = Sandbox::new();

    std::fs::create_dir_all(sb.path().join(".kymido")).expect("建 .kymido 失败");
    std::fs::write(
        sb.path().join(".kymido/config.toml"),
        "data_dir = \"./.kymido\"\n\
         model = \"m\"\n\
         \n\
         [[mcp.servers]]\n\
         name = \"ghost\"\n\
         command = \"ghost-server\"\n\
         env = { GHOST = \"1\" }\n\
         some_unknown_key = \"keep-me\"\n",
    )
    .expect("写配置失败");

    let mut loaded = LlmRuntimeConfig::load_from_system();
    // 表单里只有一台 fs；ghost 不在表单里。
    loaded.mcp_servers = vec![McpServerForm {
        name: "fs".to_string(),
        command: "npx".to_string(),
        url: String::new(),
        args: String::new(),
        cwd: String::new(),
        tool_call_timeout_ms: String::new(),
        fail_on_startup_error: false,
    }];
    loaded.save_to_file().expect("保存配置失败");

    let doc = std::fs::read_to_string(sb.path().join(".kymido/config.toml"))
        .expect("读回配置失败")
        .parse::<toml_edit::DocumentMut>()
        .expect("保存后的配置必须是合法 TOML");
    let servers = doc["mcp"]["servers"]
        .as_array_of_tables()
        .expect("[[mcp.servers]] 应是表数组");
    assert_eq!(servers.len(), 2, "表单外的服务器不得被删");
    let by_name = |n: &str| -> Option<&toml_edit::Table> {
        servers
            .iter()
            .find(|t| t.get("name").and_then(toml_edit::Item::as_str) == Some(n))
    };
    let ghost = by_name("ghost").expect("ghost 服务器应保留");
    assert_eq!(ghost["command"].as_str(), Some("ghost-server"));
    assert!(ghost.get("env").is_some(), "ghost 的 env 应保留");
    assert!(
        ghost.get("some_unknown_key").is_some(),
        "ghost 的未知键应保留"
    );
}
