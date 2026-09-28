//! Internal TOML wire-format mirrors for deserialization.
//!
//! The public [`crate::Config`] is a validated, filled-in value; these are
//! all-optional mirrors that `serde` fills from the raw TOML and then
//! `merge_into` folds onto a base `Config`. Keeping the wire shape in its own
//! module isolates the two config representations from each other.
//!
//! Module named `wire` (not `toml`) so it does not shadow the external `toml`
//! crate that `Config::load` calls `from_str` on.

use std::path::PathBuf;

use crate::{Config, McpServerConfig, SubagentProviderConfig};

/// Internal TOML config struct for deserialization (all fields optional).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct TomlConfig {
    omp_path: Option<String>,
    data_dir: Option<String>,
    model: Option<String>,
    /// The `[llm]` section is retired (providers moved to `providers.toml`).
    /// Captured as an opaque value so its mere presence is a load error with
    /// a migration hint (D8'), never a silent alias.
    #[serde(default)]
    llm: Option<toml::Value>,
    #[serde(default)]
    mcp: McpToml,
    #[serde(default)]
    memory: MemoryToml,
    #[serde(default)]
    daemon: DaemonToml,
    #[serde(default)]
    subagent: SubagentToml,
    #[serde(default)]
    tui: TuiToml,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct MemoryToml {
    enabled: Option<bool>,
    dir: Option<String>,
}

/// `[daemon]` TOML section: orbit-engine session knobs.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DaemonToml {
    /// Session working directory (`AGENTS.md` discovery root).
    cwd: Option<String>,
    /// Cap on LLM round-trips per run.
    max_turns: Option<u64>,
}

/// `[tui]` TOML section: terminal front-end knobs (T15 completion
/// notification). Absent field keeps the default (off).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct TuiToml {
    /// Emit an OSC9 notification (besides the bell) when a long turn
    /// batch completes.
    notify_osc9: Option<bool>,
    /// T17：TUI 起始配色方案（`"dark"` / `"light"`；缺项保持默认）。
    theme: Option<String>,
}

/// `[mcp]` TOML section. `servers` is a list of `[[mcp.servers]]` tables.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct McpToml {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

/// `[subagent]` TOML section. `providers` is a list of
/// `[[subagent.providers]]` tables.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct SubagentToml {
    #[serde(default)]
    providers: Vec<SubagentProviderConfig>,
}

impl TomlConfig {
    pub(crate) fn merge_into(self, mut base: Config) -> Config {
        if let Some(v) = self.omp_path {
            base.omp_path = PathBuf::from(v);
        }
        if let Some(v) = self.data_dir {
            base.data_dir = PathBuf::from(v);
        }
        if let Some(v) = self.model {
            base.model = v;
        }
        if !self.mcp.servers.is_empty() {
            // Normalize the transport fields here, at the merge boundary.
            // `validate` trims only to *test* a value, so a padded
            // `command = " npx "` would pass there and still be handed to
            // `Command::new` with its spaces — which then fails to spawn.
            base.mcp_servers = self
                .mcp
                .servers
                .into_iter()
                .map(|mut s| {
                    s.name = s.name.trim().to_string();
                    s.command = s.command.map(|c| c.trim().to_string());
                    s.url = s.url.map(|u| u.trim().to_string());
                    s
                })
                .collect();
        }
        // Same empty-does-not-override + merge-boundary trim as
        // `mcp.servers` above: `validate` only *tests* the trimmed value, so
        // a padded `command = " /bin/echo "` would pass and still spawn with
        // its spaces.
        if !self.subagent.providers.is_empty() {
            base.subagent_providers = self
                .subagent
                .providers
                .into_iter()
                .map(|mut p| {
                    p.name = p.name.trim().to_string();
                    p.command = p.command.trim().to_string();
                    p.cwd = p.cwd.map(|c| c.trim().to_string());
                    p
                })
                .collect();
        }
        if let Some(v) = self.memory.enabled {
            base.memory_enabled = v;
        }
        if let Some(v) = self.memory.dir {
            base.memory_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = self.daemon.cwd {
            base.cwd = PathBuf::from(v);
        }
        if let Some(v) = self.daemon.max_turns {
            // `as usize` would silently truncate on a 32-bit host; the TOML
            // field is u64 because TOML integers are. usize::try_from keeps a
            // too-large value out of the config instead of wrapping it.
            base.max_turns = usize::try_from(v).ok();
        }
        if let Some(v) = self.tui.notify_osc9 {
            base.tui_notify_osc9 = v;
        }
        if let Some(v) = self.tui.theme {
            base.tui_theme = v;
        }
        base
    }

    /// True when the retired `[llm]` section is present in the file.
    /// `Config::load` turns this into a hard error with a migration hint.
    pub(crate) fn has_legacy_llm(&self) -> bool {
        self.llm.is_some()
    }
}
