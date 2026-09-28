//! End-to-end tests for `kymido profile` (boot/bundle profiles).
//!
//! Real `kymido` binary as a subprocess in a tempdir cwd; no daemon, no API keys
//! — `profile` only touches `.kymido/config.toml` + `.kymido/providers.toml`

use std::process::{Command, Stdio};

use tempfile::TempDir;

fn run_oi(cwd: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let exe = env!("CARGO_BIN_EXE_kymido");
    let output = Command::new(exe)
        .args(args)
        .current_dir(cwd)
        // Keep the ambient environment from steering the run: data dir and
        // socket live inside the tempdir.
        .env("KYMIDO_DATA_DIR", cwd.join(".kymido"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn kymido");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn profile_list_shows_both_profiles() {
    let dir = TempDir::new().unwrap();
    let (stdout, _stderr, ok) = run_oi(dir.path(), &["profile", "list"]);
    assert!(ok, "profile list must succeed");
    assert!(stdout.contains("boot"), "got: {stdout}");
    assert!(stdout.contains("bundle"), "got: {stdout}");
}

#[test]
fn profile_apply_writes_a_loadable_config() {
    let dir = TempDir::new().unwrap();
    let (stdout, _stderr, ok) = run_oi(dir.path(), &["profile", "apply", "boot"]);
    assert!(ok, "apply must succeed: {stdout}");
    let config_content = std::fs::read_to_string(dir.path().join(".kymido/config.toml")).unwrap();
    assert!(config_content.contains("omp_path = \"omp\""));
    assert!(config_content.contains("[daemon]"));
    assert!(
        !config_content.contains("[llm]"),
        "profile must not emit a [llm] section: {config_content}"
    );
    // The written profile must be valid TOML with the sections a real run
    // reads — a profile that does not parse is not a starting point.
    let table: toml::Table =
        toml::from_str(&config_content).expect("config profile TOML must parse");
    assert!(table.contains_key("daemon"));
    // The providers side is written as a set: a loadable profile carries a
    // matching providers.toml (parseable, non-empty active route table).
    let providers_content =
        std::fs::read_to_string(dir.path().join(".kymido/providers.toml")).unwrap();
    let ptable: toml::Table =
        toml::from_str(&providers_content).expect("providers profile TOML must parse");
    assert!(
        ptable.contains_key("providers"),
        "providers side must carry a providers table: {providers_content}"
    );
}

#[test]
fn profile_apply_refuses_to_overwrite_existing_config() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".kymido")).unwrap();
    std::fs::write(dir.path().join(".kymido/config.toml"), "model = \"mine\"\n").unwrap();
    let (_stdout, stderr, ok) = run_oi(dir.path(), &["profile", "apply", "boot"]);
    assert!(!ok, "apply over an existing config must fail");
    assert!(stderr.contains("refusing to overwrite"), "got: {stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".kymido/config.toml")).unwrap(),
        "model = \"mine\"\n"
    );
}

#[test]
fn profile_apply_refuses_when_only_providers_exists() {
    let dir = TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".kymido")).unwrap();
    // 半套：只有 providers.toml（config.toml 缺席）也必须拒绝——profile 是
    // 成套写，半套会让 active 路由解析到旧的/缺省凭据。
    std::fs::write(
        dir.path().join(".kymido/providers.toml"),
        "active = \"mine\"\n",
    )
    .unwrap();
    let (_stdout, stderr, ok) = run_oi(dir.path(), &["profile", "apply", "boot"]);
    assert!(!ok, "apply over an existing providers.toml must fail");
    assert!(stderr.contains("refusing to overwrite"), "got: {stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".kymido/providers.toml")).unwrap(),
        "active = \"mine\"\n"
    );
    assert!(!dir.path().join(".kymido/config.toml").exists());
}

#[test]
fn profile_apply_unknown_name_lists_available() {
    let dir = TempDir::new().unwrap();
    let (_stdout, stderr, ok) = run_oi(dir.path(), &["profile", "apply", "nope"]);
    assert!(!ok, "unknown profile must fail");
    assert!(stderr.contains("unknown profile"), "got: {stderr}");
    assert!(stderr.contains("boot"), "got: {stderr}");
    assert!(stderr.contains("bundle"), "got: {stderr}");
    assert!(!dir.path().join(".kymido/config.toml").exists());
}
