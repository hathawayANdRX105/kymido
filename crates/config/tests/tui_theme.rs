//! `[tui] theme` 的读取与回落（T17）。
//!
//! Red when: 配置缺 `theme` 段时不是 `"dark"`，或写了个不认识的方案名时
//! TUI 启动会拿到空串 / panic —— 过期配置不该让终端前端起不来。
//!
//! `Config::load` 读进程 cwd 下的 `.kymido/config.toml`，所以这些测试不能并行
//! （沿 `profile_status.rs` 的 `cwd_lock` 口径）。

use std::sync::{LazyLock, Mutex, MutexGuard};

use config::Config;

fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `[tui]` 段缺 `theme` → 默认 `"dark"`（缺项不许落空串：那会让 TUI 的
/// 方案查找拿到 `""`，面板与状态行都无从显示）。
#[test]
fn tui_theme_defaults_to_dark() {
    let _guard = cwd_lock();
    let cfg = Config::load().expect("config loads without a file");
    assert_eq!(cfg.tui_theme, "dark");
}

/// 写了认识的方案名 → 原样读到（`light` 是首版的第二档）。
#[test]
fn tui_theme_reads_a_known_scheme() {
    let _guard = cwd_lock();
    let dir = tempdir();
    let prev = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("enter temp dir");
    std::fs::create_dir_all(dir.path().join(".kymido")).expect("config dir");
    std::fs::write(
        dir.path().join(".kymido/config.toml"),
        "[tui]\ntheme = \"light\"\n",
    )
    .expect("write config");

    let cfg = Config::load().expect("config loads");
    assert_eq!(cfg.tui_theme, "light");

    std::env::set_current_dir(prev).expect("restore cwd");
}

/// 写了不认识的方案名 → 读到的仍是原串，但**不是**崩溃：回落由 TUI 侧的
/// `theme::set_by_name` 负责（`Scheme::from_name` 返回 `None` → 默认 dark）。
/// 这里钉的是「配置层照实读、不自作主张改写用户写的值」。
#[test]
fn tui_theme_keeps_an_unknown_value_verbatim() {
    let _guard = cwd_lock();
    let dir = tempdir();
    let prev = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("enter temp dir");
    std::fs::create_dir_all(dir.path().join(".kymido")).expect("config dir");
    std::fs::write(
        dir.path().join(".kymido/config.toml"),
        "[tui]\ntheme = \"solarized\"\n",
    )
    .expect("write config");

    let cfg = Config::load().expect("an unknown scheme must not fail the load");
    assert_eq!(cfg.tui_theme, "solarized");

    std::env::set_current_dir(prev).expect("restore cwd");
}

/// 最小临时目录（不引第三方依赖：只用 std）。
fn tempdir() -> TempDir {
    let base = std::env::temp_dir().join(format!(
        "kymido-config-tui-theme-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("temp dir");
    TempDir(base)
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
