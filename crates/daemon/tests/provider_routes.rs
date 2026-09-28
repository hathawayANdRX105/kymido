//! daemon 用 providers.toml 的 active 路由建 orbit 模型；没有路由时
//! 行为与从前一致（omp 兼容模式）。
//!
//! Red when: 路由解析后能力字段（context_window / input）没进 orbit 模型，
//! 或路由缺 key 时静默用上别的 provider 的 key。
use std::sync::{LazyLock, Mutex, MutexGuard};

use daemon::{Daemon, DaemonConfig};

/// `Config::load` reads `.kymido/config.toml` + `providers.toml` relative to
/// the *process* cwd, so these tests must not run concurrently — one test's
/// temp files would be read as another's. Serialized here so the rest of
fn cwd_lock() -> MutexGuard<'static, ()> {
    static LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write an optional `config.toml` + `providers.toml` into a temp `.kymido/`
/// dir and build the daemon config from the load result.
fn config_from(
    config_toml: Option<&str>,
    providers_toml: Option<&str>,
) -> (tempfile::TempDir, DaemonConfig) {
    let dir = tempfile::tempdir().expect("temp dir");
    let kymido = dir.path().join(".kymido");
    std::fs::create_dir_all(&kymido).expect("create .kymido");
    if let Some(body) = config_toml {
        std::fs::write(kymido.join("config.toml"), body).expect("write config");
    }
    if let Some(body) = providers_toml {
        std::fs::write(kymido.join("providers.toml"), body).expect("write providers");
    }
    let _guard = cwd_lock();
    let original = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("switch cwd");
    let result = std::panic::catch_unwind(config::Config::load);
    let _ = std::env::set_current_dir(&original);
    let cfg = result
        .expect("Config::load panicked")
        .expect("Config::load failed");
    (dir, DaemonConfig::from_config(&cfg).expect("daemon config"))
}

const PROVIDERS: &str = r#"
active = "work/model-a"

[providers.work]
base_url = "https://api.example.com"
api_key = "key-a"
default_model = "model-a"
max_tokens = 8192
fallbacks = ["other/model-b"]

[[providers.work.models]]
id = "model-a"
context_window = 100000
input = ["text", "image"]
max_tokens = 4096

[providers.other]
base_url = "https://other.example.com"
api_key = "key-b"
default_model = "model-b"
"#;

#[test]
fn active_route_reaches_the_orbit_model() {
    let (_dir, cfg) = config_from(None, Some(PROVIDERS));
    let model = cfg.orbit_model.expect("orbit model from the active route");
    assert_eq!(model.model, "model-a");
    assert_eq!(model.api_key, "key-a");
    assert_eq!(
        model.base_url.as_deref(),
        Some("https://api.example.com/v1"),
        "the `/v1` suffix is still appended at the call site"
    );
    assert_eq!(
        model.max_tokens,
        Some(4096),
        "model-level beats provider-level"
    );
    assert_eq!(
        model.context_window,
        Some(100_000),
        "the declared window rides into the orbit model"
    );
    assert!(model.image_input(), "declared image modality rides along");
}

#[test]
fn no_active_route_keeps_omp_compat_mode() {
    // A providers file with entries but no `active` key: the daemon runs
    // without an orbit model, exactly like machines that never configured
    // a direct credential.
    let (_dir, cfg) = config_from(None, Some("base = 1"));
    assert!(
        cfg.orbit_model.is_none(),
        "no active route = no orbit model"
    );
}

#[test]
fn route_without_a_resolvable_key_yields_no_orbit_model() {
    // A provider whose key comes from an unset env variable must not
    // silently borrow another provider's key: no key anywhere = no model.
    let (_dir, cfg) = config_from(
        None,
        Some(
            r#"
active = "envonly/m"

[providers.envonly]
base_url = "https://api.example.com"
api_key_env = "KYMIDO_TEST_ABSENT_ROUTE_KEY"

[providers.work]
base_url = "https://api.example.com"
api_key = "key-a"
default_model = "model-a"
"#,
        ),
    );
    assert!(
        cfg.orbit_model.is_none(),
        "no key for the active route = no orbit model"
    );
}

#[test]
fn fallback_hops_carry_target_provider_credentials() {
    let (_dir, cfg) = config_from(None, Some(PROVIDERS));
    let hops = &cfg.llm_fallbacks;
    assert_eq!(hops.len(), 1, "one resolvable fallback hop");
    assert_eq!(hops[0].model, "model-b");
    assert_eq!(
        hops[0].api_key, "key-b",
        "the hop dials the target provider's own key"
    );
    assert_eq!(hops[0].base_url, "https://other.example.com");
}

#[test]
fn daemon_starts_with_a_providers_only_config() {
    let (dir, cfg) = config_from(None, Some(PROVIDERS));
    let mut cfg = cfg;
    // Point the daemon at the temp dir: the default `data_dir` is a path
    // under the user's home, which a fresh CI runner has never created, and
    // this test is about the credential shape, not about the default dir.
    cfg.data_dir = dir.path().to_path_buf();
    cfg.socket_path = Some(cfg.data_dir.join("route-daemon.sock"));
    let _daemon = Daemon::start(cfg).expect("daemon starts on a providers-only config");
}
