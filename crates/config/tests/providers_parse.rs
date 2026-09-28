//! providers.toml — 路由解析、能力继承、typo 拒载、legacy `[llm]` 拒绝、
//! fallback 继承与 skip。
//!
//! Red when: 路由解析静默回落到别的 provider、能力字段没从 model 条目继承、
//! legacy `[llm]` 段没被拒载、未知 fallback 路由被静默吞掉。

use std::sync::{LazyLock, Mutex, MutexGuard};

use config::{Config, ProviderStatus};

/// `Config::load` 读进程 cwd 下的 `.kymido/config.toml` 与 `providers.toml`，
/// 且 env 覆盖是进程级——全部用例串行（同 `mcp_config_validate.rs` 的 cwd_lock 口径）。
fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 在 tempdir 写 `providers.toml`（可选再写 config.toml），切 cwd 加载，
/// 恢复 cwd 后返回 Config。调用方必须已持有 `cwd_lock`——锁不可重入，
/// 本函数自身不再抢锁（曾有两个 env 测试持锁再走 `load_providers`，
/// 双重抢锁把整个二进制吊死在 CI 的 25 分钟超时上）。
fn load_providers_held(providers_toml: Option<&str>, config_toml: Option<&str>) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    if let Some(body) = providers_toml {
        std::fs::write(dir.path().join("providers.toml"), body).expect("write providers");
    }
    if let Some(body) = config_toml {
        std::fs::write(dir.path().join("config.toml"), body).expect("write config");
    }
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let result = std::panic::catch_unwind(config::Config::load);
    std::env::set_current_dir(&original).expect("restore cwd");
    match result {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => panic!("Config::load failed: {e}"),
        Err(_) => panic!("Config::load panicked"),
    }
}

/// 不直接碰 env 变量的便捷包装：加载期间持锁。碰 env 变量的测试自己
/// 持锁（set_var…remove_var 全程串行化），改调 `load_providers_held`。
fn load_providers(providers_toml: Option<&str>, config_toml: Option<&str>) -> Config {
    let _guard = cwd_lock();
    load_providers_held(providers_toml, config_toml)
}

const FILE: &str = r#"
active = "deepseek/deepseek-chat"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key_env = "KYMIDO_TEST_DEEPSEEK_KEY"
default_model = "deepseek-chat"
max_tokens = 8192
fallbacks = ["openai/gpt-4o-mini", "ghost/mystery"]

[[providers.deepseek.models]]
id = "deepseek-chat"
context_window = 65536
input = ["text"]
max_tokens = 4096

[[providers.deepseek.models]]
id = "deepseek-vision"
context_window = 65536
input = ["text", "image"]

[providers.openai]
base_url = "https://api.openai.com"
api_key = "sk-openai"
default_model = "gpt-4o-mini"

[[providers.openai.models]]
id = "gpt-4o-mini"
input = ["text", "image"]
"#;

/// 路由 "prov/model" 命中 model 条目：能力字段按 model > provider 继承。
#[test]
fn active_route_resolves_with_model_level_capabilities() {
    let _guard = cwd_lock();
    // FILE 的 deepseek 走 api_key_env：设好变量才能解析出凭据，
    // 能力断言才有的跑；跑完清掉。
    // SAFETY: cwd_lock 串行化 env 读写。
    unsafe {
        std::env::set_var("KYMIDO_TEST_DEEPSEEK_KEY", "sk-locked");
    }
    let c = load_providers_held(Some(FILE), None);
    let llm = c.active_llm().expect("active route must resolve");
    assert_eq!(llm.model, "deepseek-chat");
    assert_eq!(llm.base_url, "https://api.deepseek.com");
    assert_eq!(llm.context_window, Some(65_536));
    assert_eq!(
        llm.max_tokens,
        Some(4_096),
        "model-level beats provider-level"
    );
    assert!(!llm.image_input(), "deepseek-chat is text-only");
    unsafe {
        std::env::remove_var("KYMIDO_TEST_DEEPSEEK_KEY");
    }
}

/// 路由 "prov"（不带 model）走 provider 的 default_model。
#[test]
fn bare_route_falls_back_to_default_model() {
    let c = load_providers(
        Some(
            "active = \"openai\"\n\n[providers.openai]\nbase_url = \"https://api.openai.com\"\napi_key = \"k\"\ndefault_model = \"gpt-4o-mini\"\n",
        ),
        None,
    );
    let llm = c
        .active_llm()
        .expect("bare route must resolve via default_model");
    assert_eq!(llm.model, "gpt-4o-mini");
}

/// 缺省 input = ["text"]：没写 modality 的条目不算视觉模型。
#[test]
fn absent_input_modality_defaults_to_text_only() {
    let c = load_providers(
        Some(
            "active = \"p/m\"\n\n[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"m\"\n",
        ),
        None,
    );
    let llm = c.active_llm().expect("resolve");
    assert_eq!(llm.input, vec!["text".to_string()]);
    assert!(!llm.image_input());
}

/// 声明 image 的 model 条目放出 image_input。
#[test]
fn declared_image_modality_is_visible() {
    let c = load_providers(
        Some(
            "active = \"p/vision\"\n\n[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"vision\"\ninput = [\"text\", \"image\"]\n",
        ),
        None,
    );
    assert!(c.active_llm().expect("resolve").image_input());
}

/// api_key_env 变量未设且无 inline key → active_llm 为 None（daemon 走 omp
/// 兼容模式，不猜凭据）。
#[test]
fn unresolved_key_yields_no_credential() {
    let _guard = cwd_lock();
    // 清掉测试环境里可能残留的变量（进程级 env，不清会飘）。
    // SAFETY: cwd_lock 保证此刻没有其他测试线程在读写该变量。
    unsafe {
        std::env::remove_var("KYMIDO_TEST_DEEPSEEK_KEY");
    }
    let c = load_providers_held(Some(FILE), None);
    assert!(
        c.active_llm().is_none(),
        "no key resolvable -> no orbit model"
    );
    // 但 provider 状态能单独报出来：missing-key。
    let statuses = c.provider_statuses();
    let deepseek = statuses.iter().find(|(n, _)| *n == "deepseek").unwrap();
    assert_eq!(deepseek.1, ProviderStatus::MissingKey);
}

/// 变量设了就 ready；provider 级 key 缺省回落逻辑不受影响。
#[test]
fn set_api_key_env_counts_as_ready() {
    let _guard = cwd_lock();
    // SAFETY: 同上。
    unsafe {
        std::env::set_var("KYMIDO_TEST_DEEPSEEK_KEY", "sk-from-env");
    }
    let c = load_providers_held(Some(FILE), None);
    // 凭据在 env 变量仍在时解析（resolve 读 env 是调用时语义）。
    // status 同样是调用时语义：必须在清变量之前查。
    let llm = c.active_llm().expect("env key resolves the route");
    assert_eq!(
        c.provider_statuses()
            .iter()
            .find(|(n, _)| *n == "deepseek")
            .unwrap()
            .1,
        ProviderStatus::Ready
    );
    // SAFETY: 同上；离开前清掉，避免污染同进程后续用例。
    unsafe {
        std::env::remove_var("KYMIDO_TEST_DEEPSEEK_KEY");
    }
    assert_eq!(llm.api_key, "sk-from-env");
}

/// active 路由指向不存在的 provider = 拒载（凭据 typo 保护）。
#[test]
fn active_route_typo_is_a_load_error() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("providers.toml"),
        "active = \"deepseek/deepseek-chat\"\n\n[providers.openai]\nbase_url = \"https://api.openai.com\"\napi_key = \"k\"\n",
    )
    .expect("write");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject an unknown provider");
    std::env::set_current_dir(&original).expect("restore cwd");
    let msg = err.to_string();
    assert!(msg.contains("no such provider"), "got: {msg}");
}

/// active 路由点名的 model 不在该 provider 声明表里 = 拒载。
#[test]
fn unknown_model_in_declared_table_is_a_load_error() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("providers.toml"),
        "active = \"p/ghost\"\n\n[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"real\"\n",
    )
    .expect("write");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject an undeclared model");
    std::env::set_current_dir(&original).expect("restore cwd");
    assert!(err.to_string().contains("no model `ghost`"), "got: {err}");
}

/// 未声明 models 表的 provider：任意 model id 原样放行（私有端点透传）。
#[test]
fn undeclared_model_table_passes_ids_through() {
    let c = load_providers(
        Some(
            "active = \"p/anything-goes\"\n\n[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n",
        ),
        None,
    );
    assert_eq!(c.active_llm().expect("passthrough").model, "anything-goes");
}

/// legacy `[llm]` 段 = 拒载，报错带迁移指引（D8' 无别名）。
#[test]
fn legacy_llm_section_is_rejected_with_migration_hint() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let kymido = dir.path().join(".kymido");
    std::fs::create_dir_all(&kymido).expect("mkdir");
    std::fs::write(
        kymido.join("config.toml"),
        "[llm]\nbase_url = \"http://x\"\napi_key = \"k\"\nmodel = \"m\"\n",
    )
    .expect("write legacy config");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject the legacy [llm] section");
    std::env::set_current_dir(&original).expect("restore cwd");
    let msg = err.to_string();
    assert!(msg.contains("retired"), "got: {msg}");
    assert!(msg.contains("providers.toml"), "got: {msg}");
}

/// model id 在 provider 内重复 = 拒载。
#[test]
fn duplicate_model_ids_are_rejected() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("providers.toml"),
        "[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"same\"\n\n[[providers.p.models]]\nid = \"same\"\n",
    )
    .expect("write");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject duplicate model ids");
    std::env::set_current_dir(&original).expect("restore cwd");
    assert!(err.to_string().contains("duplicate model id"), "got: {err}");
}

/// context_window = 0 = 拒载（必须为正）。
#[test]
fn zero_context_window_is_rejected() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("providers.toml"),
        "[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"m\"\ncontext_window = 0\n",
    )
    .expect("write");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject a zero window");
    std::env::set_current_dir(&original).expect("restore cwd");
    assert!(err.to_string().contains("positive"), "got: {err}");
}

/// 未知 input 模态 = 拒载（值域 text/image）。
#[test]
fn unknown_input_modality_is_rejected() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("providers.toml"),
        "[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n\n[[providers.p.models]]\nid = \"m\"\ninput = [\"text\", \"audio\"]\n",
    )
    .expect("write");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let err = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .err()
        .expect("must reject an unknown modality");
    std::env::set_current_dir(&original).expect("restore cwd");
    assert!(
        err.to_string().contains("unknown input modality"),
        "got: {err}"
    );
}

/// 瀑布：active provider 的 fallbacks 按序解析；未知路由 warn-skip，
/// 不炸 daemon。
#[test]
fn waterfall_hops_inherit_target_provider_credentials() {
    let _guard = cwd_lock();
    // SAFETY: cwd_lock 串行化 env 读写。
    unsafe {
        std::env::set_var("KYMIDO_TEST_DEEPSEEK_KEY", "sk-primary");
    }
    let c = load_providers_held(Some(FILE), None);
    unsafe {
        std::env::remove_var("KYMIDO_TEST_DEEPSEEK_KEY");
    }
    let hops = c.fallback_llms();
    assert_eq!(hops.len(), 1, "unknown hop `ghost/mystery` is skipped");
    assert_eq!(hops[0].model, "gpt-4o-mini");
    assert_eq!(
        hops[0].api_key, "sk-openai",
        "key comes from the target provider"
    );
    assert_eq!(hops[0].base_url, "https://api.openai.com");
    assert!(hops[0].image_input());
}

/// 没有 active 路由时没有瀑布（primary 都没有，fallback 无从谈起）。
#[test]
fn no_active_route_means_no_waterfall() {
    let c = load_providers(
        Some("[providers.p]\nbase_url = \"http://x\"\napi_key = \"k\"\n"),
        None,
    );
    assert!(c.fallback_llms().is_empty());
}

/// providers.toml 与 config.toml 各自影子：工作区 providers 压住 root 的。
#[test]
fn workspace_providers_shadow_root_level() {
    let _guard = cwd_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    // .kymido/providers.toml 生效；根目录 providers.toml 被压住。
    let kymido = dir.path().join(".kymido");
    std::fs::create_dir_all(&kymido).expect("mkdir");
    std::fs::write(
        kymido.join("providers.toml"),
        "active = \"w/m\"\n\n[providers.w]\nbase_url = \"http://workspace\"\napi_key = \"kw\"\n",
    )
    .expect("write workspace providers");
    std::fs::write(
        dir.path().join("providers.toml"),
        "active = \"r/m\"\n\n[providers.r]\nbase_url = \"http://root\"\napi_key = \"kr\"\n",
    )
    .expect("write root providers");
    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let c = std::panic::catch_unwind(config::Config::load)
        .expect("no panic")
        .expect("load");
    std::env::set_current_dir(&original).expect("restore cwd");
    assert_eq!(
        c.active_llm().expect("workspace route").base_url,
        "http://workspace"
    );
}

/// KYMIDO_LLM_* 覆盖 active provider：api_key_env 已声明时 env key 不生效
/// （provider 本地声明赢全局覆盖），其余字段照旧覆盖。
#[test]
fn env_overrides_respect_local_key_declarations() {
    let _guard = cwd_lock();
    // SAFETY: cwd_lock 串行化 env 读写。
    unsafe {
        std::env::set_var("KYMIDO_TEST_DEEPSEEK_KEY", "sk-from-provider-env");
        std::env::set_var("KYMIDO_LLM_API_KEY", "sk-from-global-env");
        std::env::set_var("KYMIDO_LLM_BASE_URL", "http://env-override");
    }
    let c = load_providers_held(Some(FILE), None);
    // 凭据在 env 变量仍在时解析（resolve 读 env 是调用时语义），再清理。
    let llm = c.active_llm().expect("resolve");
    unsafe {
        std::env::remove_var("KYMIDO_TEST_DEEPSEEK_KEY");
        std::env::remove_var("KYMIDO_LLM_API_KEY");
        std::env::remove_var("KYMIDO_LLM_BASE_URL");
    }
    assert_eq!(
        llm.api_key, "sk-from-provider-env",
        "the provider's api_key_env variable beats the global override"
    );
    assert_eq!(llm.base_url, "http://env-override");
}

/// CI smoke fixture form: an empty provider row (no base_url/api_key/
/// default_model) with the whole credential injected by `KYMIDO_LLM_*`.
/// Post-cutover, env vars alone can no longer mint a credential — this is
/// the shape the CI TUI smoke steps rely on to get LLM traffic up.
#[test]
fn env_vars_fill_empty_provider_row() {
    let _guard = cwd_lock();
    // SAFETY: cwd_lock serializes env reads/writes.
    unsafe {
        std::env::set_var("KYMIDO_LLM_API_KEY", "test-key");
        std::env::set_var("KYMIDO_LLM_BASE_URL", "http://127.0.0.1:18099");
        std::env::set_var("KYMIDO_LLM_MODEL", "test-model");
    }
    let c = load_providers_held(
        Some(
            r#"
active = "smoke"

[providers.smoke]
"#,
        ),
        None,
    );
    let llm = c.active_llm().expect("empty row + env vars must resolve");
    unsafe {
        std::env::remove_var("KYMIDO_LLM_API_KEY");
        std::env::remove_var("KYMIDO_LLM_BASE_URL");
        std::env::remove_var("KYMIDO_LLM_MODEL");
    }
    assert_eq!(llm.model, "test-model");
    assert_eq!(llm.base_url, "http://127.0.0.1:18099");
    assert_eq!(llm.api_key, "test-key");
}
