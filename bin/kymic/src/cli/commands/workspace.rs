//! init / profile / template subcommand handlers, split out of `work.rs`.

use super::*;

use config::Config;
use store::store::Store;

pub fn init_cmd(json: bool) -> Result<u8, String> {
    let dir = std::env::current_dir().map_err(|e| format!("cwd error: {e}"))?;
    init_cmd_at(&dir, json)
}

/// init internals, testable with an explicit workspace directory.
///
/// Splits the two concerns that used to share one directory:
/// - **user-scoped** (`Config::config_dir`, normally `~/.config/kymido`):
///   `config.toml` plus the `specs/` and `templates/` seed files. These follow
///   the user across repositories, so they do not belong to a workspace.
/// - **workspace-scoped** (`<dir>/.kymido`): the work-item store the CLI and
///   daemon append to (`tasks.jsonl` / `todos.jsonl` / `goals.jsonl`). A task
///   list describes one repository's work, so it stays next to it.
pub fn init_cmd_at(dir: &std::path::Path, json: bool) -> Result<u8, String> {
    let config_dir = Config::ensure_config_dir().map_err(|e| format!("config dir: {e}"))?;
    let config_path = Config::config_file().map_err(|e| format!("config file: {e}"))?;
    let config_existed = config_path.exists();

    // A workspace-local config predates the move to XDG. Copy it across rather
    // than leaving the user to re-enter their API key, then rename the source
    // aside: `Config::load` lets a workspace file override the user-scoped one,
    // so leaving it in place would keep the migrated copy permanently shadowed
    // by the stale original. A rename, not a delete — nothing is lost.
    let legacy = dir.join(".kymido/config.toml");
    let mut migrated = None;
    if !config_existed && legacy.is_file() {
        std::fs::copy(&legacy, &config_path).map_err(|e| {
            format!(
                "migrate {} -> {}: {e}",
                legacy.display(),
                config_path.display()
            )
        })?;
        let aside = dir.join(".kymido/config.toml.migrated");
        std::fs::rename(&legacy, &aside).map_err(|e| {
            format!(
                "could not move the old config aside ({} -> {}): {e}; delete it or it will keep shadowing the migrated copy",
                legacy.display(),
                aside.display()
            )
        })?;
        migrated = Some(aside);
    } else if !config_existed {
        let default = "omp_path = \"omp\"\n\
                       model = \"default\"\n";
        std::fs::write(&config_path, default)
            .map_err(|e| format!("could not create {}: {e}", config_path.display()))?;
    }
    // Spec templates (never overwrite user edits).
    store::specs::init::write_default_specs(&config_dir)
        .map_err(|e| format!("spec templates: {e}"))?;
    // Task templates (never overwrite user edits).
    store::template::write_default_templates(&config_dir)
        .map_err(|e| format!("task templates: {e}"))?;

    // The work-item store is created lazily by `Store::new`; only the config
    // side needs laying down here.
    let msg = if let Some(aside) = migrated {
        format!(
            "migrated {} to {} (original kept as {})",
            legacy.display(),
            config_path.display(),
            aside.display()
        )
    } else if config_existed {
        format!("already initialized: {}", config_path.display())
    } else {
        format!("initialized: {}", config_path.display())
    };
    if json {
        json_ok(&msg);
    } else {
        println!("{msg}");
    }
    Ok(0)
}

/// Embedded boot/bundle profiles. Files live at the repo root `profiles/`;
/// embedded (not read from disk) so the binary works from any cwd.
const PROFILES: &[(&str, &str)] = &[
    ("boot", include_str!("../../../../../profiles/boot.toml")),
    (
        "bundle",
        include_str!("../../../../../profiles/bundle.toml"),
    ),
];

/// `profile list` -- 名字 + 文件首行描述。
pub fn profile_list_cmd(json: bool) -> Result<u8, String> {
    let rows: Vec<(&str, &str)> = PROFILES
        .iter()
        .map(|(name, body)| (*name, profile_blurb(body)))
        .collect();
    if json {
        let value: Vec<serde_json::Value> = rows
            .iter()
            .map(|(name, blurb)| serde_json::json!({ "name": name, "description": blurb }))
            .collect();
        print_json(&value);
    } else {
        for (name, blurb) in &rows {
            println!("{name}\t{blurb}");
        }
    }
    Ok(0)
}

/// 文件首条 `#` 注释即描述（ profiles 的约定：第一行写用途）。
pub fn profile_blurb(body: &str) -> &str {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .unwrap_or("")
        .trim()
}

/// `profile apply <name>` -- 把 profile 写进 [`Config::config_file`]。
/// 与 `init` 同一语义：已存在则拒绝覆盖（返回非零），不动用户配置。
pub fn profile_apply_cmd_at(_dir: &std::path::Path, name: &str, json: bool) -> Result<u8, String> {
    let Some((_, body)) = PROFILES.iter().find(|(n, _)| *n == name) else {
        return Err(format!(
            "unknown profile '{name}' (available: {})",
            PROFILES
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    Config::ensure_config_dir().map_err(|e| format!("config dir: {e}"))?;
    let config_path = Config::config_file().map_err(|e| format!("config file: {e}"))?;
    if config_path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite",
            config_path.display()
        ));
    }
    std::fs::write(&config_path, body)
        .map_err(|e| format!("could not write {}: {e}", config_path.display()))?;
    let msg = format!("applied profile '{name}': {}", config_path.display());
    if json {
        json_ok(&msg);
    } else {
        println!("{msg}");
    }
    Ok(0)
}

/// `ready` -- list open tasks whose deps are all done, sorted by priority then id.
pub fn template_list_cmd(json: bool) -> Result<u8, String> {
    let config = Config::load().map_err(|e| format!("config error: {e}"))?;
    let templates = store::template::load_all_templates(&config.data_dir)
        .map_err(|e| format!("template error: {e}"))?;
    if templates.is_empty() {
        eprintln!(
            "no templates in {} (run `cli init` first)",
            config.data_dir.join("templates").display()
        );
        return Ok(1);
    }
    if json {
        let list: Vec<serde_json::Value> = templates
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "kind": match t.kind {
                        store::template::TemplateKind::Phase => "phase",
                        store::template::TemplateKind::Step => "step",
                    },
                    "tasks": t.tasks.iter().map(|x| x.key.clone()).collect::<Vec<_>>(),
                })
            })
            .collect();
        print_json(&list);
    } else {
        for t in &templates {
            let kind = match t.kind {
                store::template::TemplateKind::Phase => "phase",
                store::template::TemplateKind::Step => "step",
            };
            println!(
                "{kind}: {} — {}",
                t.name,
                t.tasks.first().map(|x| x.title.as_str()).unwrap_or("")
            );
            for x in &t.tasks {
                println!("  - {}", x.key);
            }
        }
    }
    Ok(0)
}

/// `template apply` subcommand: create topic + phase + steps from a template.
pub fn template_apply_cmd(
    store: &Store,
    name: &str,
    topic: &str,
    parent: Option<String>,
    json: bool,
) -> Result<u8, String> {
    let config = Config::load().map_err(|e| format!("config error: {e}"))?;
    let ids = store::template::apply(store, &config.data_dir, name, topic, parent)
        .map_err(|e| format!("template error: {e} — try `cli template list` or `cli init`"))?;
    if json {
        let obj = serde_json::json!({ "created": ids });
        print_json(&obj);
    } else {
        for id in &ids {
            println!("created {id}");
        }
    }
    Ok(0)
}
