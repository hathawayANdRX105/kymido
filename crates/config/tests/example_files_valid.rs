//! 示例文件即 fixture：`config/*.example.toml`（仓库根提交的示例）必须能
//! 走真实 `Config::load` 加载成功——wire 字段改名、枚举值漂移（如
//! subagent permission 写 "prompt"）、combos 目标断链，任何一处都会让
//! 本测试红。示例文档从此与加载器强同步。
//!
//! Red when: 示例文件解析失败、validate（含 combos 强制检查）拒载、
//! active combo 或瀑布 combo hop 解析不出示例钉死的模型与能力值。

use std::sync::{LazyLock, Mutex, MutexGuard};

use config::Config;

/// `Config::load` 读进程 cwd 下的 `.kymido/`，且 env 是进程级——
/// 全部用例串行（同 `providers_parse.rs` 的 cwd_lock 口径）。
fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 测试二进制 cwd = crate 目录（crates/config）→ 上两级 = 仓库根。
fn repo_root() -> std::path::PathBuf {
    let mut p = std::env::current_dir().expect("current_dir");
    for _ in 0..2 {
        p = p.parent().expect("parent").to_path_buf();
    }
    p
}

/// 两份示例文件拷进临时 .kymido/，真实 Config::load 成功（validate +
/// combos 强制检查全跑通），且解析结果与示例钉死的语义一致。
#[test]
fn example_files_load_and_resolve() {
    let _guard = cwd_lock();
    let root = repo_root();
    let cfg_text = std::fs::read_to_string(root.join("config/config.example.toml"))
        .expect("read config example");
    let prv_text = std::fs::read_to_string(root.join("config/providers.example.toml"))
        .expect("read providers example");

    // 示例 deepseek 行走 api_key_env：存旧值、设 dummy、结束恢复（不清掉
    // 真实环境变量）。
    let saved_key = std::env::var("DEEPSEEK_API_KEY").ok();
    // SAFETY: cwd_lock 串行化 env 读写。
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "sk-example-test");
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let kymido = dir.path().join(".kymido");
    std::fs::create_dir_all(&kymido).expect("mkdir .kymido");
    std::fs::write(kymido.join("config.toml"), &cfg_text).expect("write config");
    std::fs::write(kymido.join("providers.toml"), &prv_text).expect("write providers");

    let original = std::env::current_dir().expect("current_dir");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let c = Config::load().expect("example files must load (validate incl. combos)");
    let llm = c.active_llm().expect("active combo `fast` must resolve");
    // active = "fast"（combo 短名）→ 钉死的底层路由。
    assert_eq!(llm.model, "deepseek-chat");
    assert_eq!(llm.context_window, Some(65_536));
    assert!(!llm.image_input(), "deepseek-chat 条目是纯文本模态");
    // 瀑布 hop 走 combo：deepseek 行 fallbacks = ["vision"]。
    let hops = c.fallback_llms();
    assert_eq!(hops.len(), 1, "combo hop `vision` must resolve");
    assert_eq!(hops[0].model, "gpt-4o-mini");
    assert!(hops[0].image_input(), "目标条目声明 image 模态");
    // 能力值跟目标 model 条目（openai/gpt-4o-mini 声明 128000）。
    assert_eq!(hops[0].context_window, Some(128_000));

    std::env::set_current_dir(&original).expect("restore cwd");
    // SAFETY: 同上；恢复原值（没有则移除）。
    unsafe {
        match saved_key {
            Some(v) => std::env::set_var("DEEPSEEK_API_KEY", v),
            None => std::env::remove_var("DEEPSEEK_API_KEY"),
        }
    }
}
