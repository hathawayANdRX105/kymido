//! Configuration: omp path / data dir / model selection.
//!
//! Implemented in M1.7 (TOML + env fallback).
//!
//! M1.7: config struct + loader.

pub mod providers;
mod wire;

pub use crate::providers::{
    MODALITY_IMAGE, MODALITY_TEXT, ModelEntry, ModelInput, ProviderEntry, ProviderStatus,
    ProvidersFile, ResolvedLlm,
};
use crate::wire::TomlConfig;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// Configuration for kymido.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path to the omp binary.
    pub omp_path: PathBuf,
    /// Directory for kymido data storage.
    pub data_dir: PathBuf,
    /// Model name to use.
    pub model: String,
    /// Provider credential + capability data, from `providers.toml`
    /// (a sibling file next to `config.toml` — see `crate::providers`).
    /// Absent/empty = no direct LLM credential (the daemon runs omp-compat
    /// mode, the historic path).
    pub providers: ProvidersFile,
    /// External MCP servers to spawn for extra tools. Empty by default —
    /// MCP is opt-in and nothing is spawned unless the user lists a server.
    pub mcp_servers: Vec<McpServerConfig>,
    /// Out-of-process subagent providers (`[[subagent.providers]]`): spawned
    /// ACP child agents the daemon can delegate runs to. Empty by default —
    /// subagents are opt-in, same as `mcp_servers`.
    pub subagent_providers: Vec<SubagentProviderConfig>,

    /// Persistent local memory. Off by default: nothing is written to disk
    /// until the user opts in via `[memory] enabled = true`.
    pub memory_enabled: bool,
    /// Override for the memory directory; `None` = `data_dir/memory`.
    pub memory_dir: Option<PathBuf>,
    /// Working directory the daemon's orbit engine treats as the session
    /// root: `AGENTS.md` workspace-instruction discovery walks up this
    /// directory's ancestor chain. Defaults to the daemon startup directory
    /// (`.kymido/config.toml` `[daemon] cwd`, or `KYMIDO_CWD`).
    pub cwd: PathBuf,
    /// Cap on LLM round-trips per run for the orbit engine. `None` = the
    /// loop's own default (`[daemon] max_turns`, or `KYMIDO_MAX_TURNS`).
    pub max_turns: Option<usize>,
    /// `[tui] notify_osc9` — besides the bell, emit an OSC9 notification
    /// when a long turn batch completes. Off by default: the bell is the
    /// primary channel and OSC9 delivery varies by terminal.
    pub tui_notify_osc9: bool,
    /// `[tui] theme` — 终端前端的起始配色方案（`"dark"` / `"light"`）。
    /// 缺省或无法识别都落回 `"dark"`，故过期配置也不会让 TUI 启动 panic。
    pub tui_theme: String,
}

/// One external MCP server: a stdio child process (`command`), or a running
/// HTTP endpoint (`url`) for the streamable-HTTP transport.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct McpServerConfig {
    /// Short handle used to namespace this server's tool names.
    pub name: String,
    /// Executable to spawn. Omit for HTTP servers (use `url` instead).
    #[serde(default)]
    pub command: Option<String>,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the child.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// HTTP endpoint for the streamable-HTTP transport.
    #[serde(default)]
    pub url: Option<String>,
    /// Per-call timeout in ms. `None` = crate default (`MCP_TIMEOUT`).
    #[serde(default)]
    pub tool_call_timeout_ms: Option<u64>,
    /// Reconnect policy. `None` = default (500ms → 30s, 10 attempts).
    #[serde(default)]
    pub reconnect: Option<McpReconnectConfig>,
    /// Working directory for the stdio child process. `None` = inherit the
    /// daemon/CLI process cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// When true, a server that fails to start/handshake aborts the whole MCP
    /// bring-up instead of being skipped. Default false (skip + log).
    #[serde(default)]
    pub fail_on_startup_error: Option<bool>,
}

/// Per-server reconnect/backoff tuning. Absent fields fall back to the
/// defaults in the mcp crate (`ReconnectPolicy::default`).
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct McpReconnectConfig {
    #[serde(default)]
    pub initial_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_delay_ms: Option<u64>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
}

/// Permission policy for ACP child requests: how the client auto-answers
/// the child agent's permission prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentPermission {
    Allow,
    Reject,
}

impl Default for SubagentPermission {
    fn default() -> Self {
        Self::Reject
    }
}

/// One out-of-process subagent provider (`[[subagent.providers]]`): a spawned
/// ACP child agent the daemon can delegate runs to. The transport is ACP for
/// now; the spawn fields (`command`/`args`/`env`/`cwd`) are kept generic so a
/// future transport reuses them without a config migration.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct SubagentProviderConfig {
    /// Registry name; must not collide with the built-in `fork`.
    pub name: String,
    /// Executable to spawn (the child ACP agent). Required.
    pub command: String,
    /// Arguments passed to `command`.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra env for the child, merged over the scrubbed parent env.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Child cwd. `None` = inherit the daemon session cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// How the client auto-answers the child's permission prompts.
    #[serde(default)]
    pub permission: SubagentPermission,
    /// Post-SIGKILL reap window on dispose, ms (`AcpProviderSpec::kill_grace`). `None` = provider default (3000).
    #[serde(default)]
    pub dispose_grace_ms: Option<u64>,
    /// stdin-EOF quiesce window on dispose, ms. `None` = provider default (6000).
    #[serde(default)]
    pub dispose_eof_grace_ms: Option<u64>,
}

/// Errors that can occur during config loading.
#[derive(Debug)]
pub enum ConfigError {
    /// I/O error (e.g., reading config file).
    Io(std::io::Error),
    /// TOML parsing error.
    Toml(toml::de::Error),
    /// Generic parse error with context.
    #[allow(dead_code)] // kept for future manual parsers; not hit by toml path
    Parse(String),
    /// Field-level validation error: invalid value for a named field.
    Invalid {
        field: &'static str,
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "I/O error: {}", e),
            ConfigError::Toml(e) => write!(f, "TOML parse error: {}", e),
            ConfigError::Parse(s) => write!(f, "Config parse error: {}", s),
            ConfigError::Invalid { field, message } => {
                write!(f, "invalid config field `{field}`: {message}")
            }
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ConfigError::Io(e) => Some(e),
            ConfigError::Toml(e) => Some(e),
            ConfigError::Parse(_) | ConfigError::Invalid { .. } => None,
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(e: toml::de::Error) -> Self {
        ConfigError::Toml(e)
    }
}

impl Config {
    /// Load configuration with priority: env > TOML file > defaults.
    ///
    /// Config lives in the `.kymido/` directory (`.kymido/config.toml`); the
    /// legacy root `kymido.toml` is still read when present so pre-.kymido
    /// workspaces keep working (migrate with `cli init`).
    pub fn load() -> Result<Config, ConfigError> {
        // Start with defaults (data lives inside the `.kymido/` config dir).
        let mut config = Config {
            omp_path: PathBuf::from("omp"),
            data_dir: PathBuf::from("./.kymido"),
            model: String::from("default"),
            providers: ProvidersFile::default(),
            mcp_servers: Vec::new(),
            subagent_providers: Vec::new(),
            memory_enabled: false,
            memory_dir: None,
            tui_theme: "dark".to_string(),
            cwd: Self::default_cwd(),
            max_turns: None,
            tui_notify_osc9: false,
        };

        // Load from TOML file (.kymido/config.toml, legacy fallback
        // kymido.toml); a missing file is not an error. The retired `[llm]`
        // section is a hard error with a migration hint, never an alias
        // (D8').
        let candidates = [
            PathBuf::from("./.kymido/config.toml"),
            PathBuf::from("./kymido.toml"),
        ];
        for toml_path in &candidates {
            if let Ok(content) = std::fs::read_to_string(toml_path) {
                let toml_config: TomlConfig = toml::from_str(&content)?;
                if toml_config.has_legacy_llm() {
                    return Err(ConfigError::Invalid {
                        field: "llm",
                        message: format!(
                            "`[llm]` in {} is retired; move the credentials to \
providers.toml ([providers.<name>] + [[providers.<name>.models]]) and delete the section",
                            toml_path.display()
                        ),
                    });
                }
                config = toml_config.merge_into(config);
                break;
            }
        }

        // `providers.toml` shadows per-file, exactly like `config.toml`:
        // workspace `.kymido/providers.toml` beats the legacy root
        // `./providers.toml`; a missing file is not an error (no direct
        // credential, the daemon runs omp-compat mode).
        let provider_candidates = [
            PathBuf::from("./.kymido/providers.toml"),
            PathBuf::from("./providers.toml"),
        ];
        for toml_path in &provider_candidates {
            if let Ok(content) = std::fs::read_to_string(toml_path) {
                let providers: ProvidersFile = toml::from_str(&content)?;
                config.providers = providers;
                break;
            }
        }

        // Environment variables override everything
        if let Ok(v) = env::var("KYMIDO_OMP_PATH") {
            config.omp_path = PathBuf::from(v);
        }
        if let Ok(v) = env::var("KYMIDO_DATA_DIR") {
            config.data_dir = PathBuf::from(v);
        }
        config
            .memory_dir
            .get_or_insert_with(|| config.data_dir.join("memory"));
        if let Ok(v) = env::var("KYMIDO_MODEL") {
            config.model = v;
        }
        // `KYMIDO_LLM_*` overrides the *active* provider's fields when one
        // is named; they cannot invent a provider on their own (a machine
        // that wants a credential declares it in `providers.toml`).
        // Key precedence chain: a provider-declared `api_key_env` variable
        // beats `KYMIDO_LLM_API_KEY`, which beats the inline `api_key` —
        // a provider-local key declaration wins over the global override.
        if let Some(route) = config.providers.active.clone()
            && let Some((provider_name, _)) = ProvidersFile::split_route(&route)
            && let Some(entry) = config.providers.providers.get_mut(&provider_name)
        {
            if let Ok(v) = env::var("KYMIDO_LLM_API_KEY")
                && entry.api_key_env.is_none()
            {
                entry.api_key = Some(v);
            }
            if let Ok(v) = env::var("KYMIDO_LLM_BASE_URL") {
                entry.base_url = Some(v);
            }
            if let Ok(v) = env::var("KYMIDO_LLM_MODEL")
                && !route.contains('/')
            {
                entry.default_model = Some(v);
            }
            if let Ok(v) = env::var("KYMIDO_LLM_MAX_TOKENS") {
                entry.max_tokens = v.parse().ok();
            }
        }
        if let Ok(v) = env::var("KYMIDO_CWD") {
            config.cwd = PathBuf::from(v);
        }
        if let Ok(v) = env::var("KYMIDO_MAX_TURNS") {
            config.max_turns = v.parse().ok();
        }
        config.validate()?;
        Ok(config)
    }

    /// The primary LLM credential: the active spec (combo name or route)
    /// resolved against the provider table. `None` when the route is
    /// missing, unresolvable, or the provider has no usable key: the
    /// daemon then runs without an orbit model rather than guessing.
    ///
    /// `Config::validate` already rejects an active spec that names a
    /// provider or model that does not exist (the typo protection of the
    /// old `active_profile` contract), so this only returns `None` for
    /// "no active spec" and "no key resolvable".
    pub fn active_llm(&self) -> Option<ResolvedLlm> {
        self.providers
            .active
            .as_deref()
            .and_then(|spec| self.providers.resolve_spec(spec))
    }

    /// Waterfall hops for the active provider, in listed order. Unknown or
    /// unresolvable routes are skipped with a warn (B2b1 semantics: a
    /// typo'd hop must stay visible, not silently take down the daemon);
    /// a hop that resolves but has no key is the same warn-and-skip.
    pub fn fallback_llms(&self) -> Vec<ResolvedLlm> {
        let Some(spec) = self.providers.active.as_deref() else {
            return Vec::new();
        };
        // A combo active spec resolves to its target route first; the
        // waterfall belongs to the provider row that route names.
        let route = self.providers.spec_to_route(spec);
        let Some((provider_name, _)) = ProvidersFile::split_route(route) else {
            return Vec::new();
        };
        let Some(entry) = self.providers.provider(&provider_name) else {
            return Vec::new();
        };
        entry
            .fallbacks
            .iter()
            .enumerate()
            .filter_map(|(i, hop)| match self.providers.resolve_spec(hop) {
                Some(resolved) => Some(resolved),
                None => {
                    eprintln!(
                        "warn: providers fallback #{i} `{hop}` does not resolve (unknown combo/provider/model, missing base_url or key); skipped"
                    );
                    None
                }
            })
            .collect()
    }

    /// Every provider with its status, name-sorted for stable logs. The
    /// daemon prints this at start so a broken provider is visible without
    /// having to select it and discover the failure at request time.
    pub fn provider_statuses(&self) -> Vec<(&str, ProviderStatus)> {
        let mut names: Vec<&String> = self.providers.providers.keys().collect();
        names.sort();
        names
            .iter()
            .map(|name| {
                (
                    name.as_str(),
                    self.providers.provider(name).unwrap().status(),
                )
            })
            .collect()
    }

    /// Daemon session working directory: the orbit engine searches this
    /// directory's ancestor chain for `AGENTS.md`. Defaults to the process
    /// working directory — the daemon startup dir — never to a silent
    /// `None`, so a workspace without an explicit `cwd` still gets instruction
    /// discovery from where it was launched.
    fn default_cwd() -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// Validate config fields after all sources are merged.
    ///
    /// - `model`: must be non-empty.
    /// - `omp_path`: if it contains a path separator, must exist and be executable;
    ///   bare command names are allowed (resolved via PATH at runtime).
    /// - `data_dir`: if it exists, must be a directory; if not, parent must exist.
    fn validate(&self) -> Result<(), ConfigError> {
        // model: non-empty (don't echo the value back — could be sensitive).
        if self.model.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "model",
                message: "must not be empty".to_string(),
            });
        }

        // omp_path: bare name = PATH lookup (allowed); path with separator = must exist.
        let omp_str = self.omp_path.to_string_lossy();
        if omp_str.contains('/') && !self.omp_path.exists() {
            return Err(ConfigError::Invalid {
                field: "omp_path",
                message: format!("'{}' does not exist", omp_str),
            });
        }

        // [combos]: every target must be a literal `provider/model` route
        // naming an existing provider row and (when that provider declares
        // a capability table) one of its models. A combo is a pinned alias
        // to a configured model: a bare-provider target or a combo-to-combo
        // value is a load error, never a silent indirection.
        let mut combo_names: Vec<&String> = self.providers.combos.keys().collect();
        combo_names.sort();
        for name in combo_names {
            let target = self.providers.combos[name].trim();
            let (prov, model) = match crate::providers::ProvidersFile::split_route(target) {
                Some(r) => r,
                None => {
                    return Err(ConfigError::Invalid {
                        field: "providers.combos",
                        message: format!(
                            "combo `{name}` target `{target}` must be `provider/model`"
                        ),
                    });
                }
            };
            let Some(m) = &model else {
                return Err(ConfigError::Invalid {
                    field: "providers.combos",
                    message: format!(
                        "combo `{name}` target must name a model (got bare provider `{prov}`)"
                    ),
                });
            };
            let entry = match self.providers.provider(&prov) {
                Some(e) => e,
                None => {
                    return Err(ConfigError::Invalid {
                        field: "providers.combos",
                        message: format!(
                            "combo `{name}` targets provider `{prov}` which is not in providers.toml"
                        ),
                    });
                }
            };
            if !entry.models.is_empty() && !entry.models.iter().any(|x| x.id == *m) {
                return Err(ConfigError::Invalid {
                    field: "providers.combos",
                    message: format!(
                        "combo `{name}` target model `{m}` is not declared by provider `{prov}` (declared: {})",
                        entry
                            .models
                            .iter()
                            .map(|x| x.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
            }
        }

        // providers.toml: the active spec must name a provider that exists,
        // and a named model must be one the provider declared. Silently
        // falling back would hand the run a *different* provider than the
        // file asked for — the worst possible failure for a credential typo.
        if let Some(spec) = self.providers.active.as_deref() {
            // A combo active spec was already enforced by the `[combos]`
            // check (explicit target, provider row, declared model): its
            // errors name the combo, not the active route. Only literal
            // and bare-provider specs are validated here.
            let is_combo = spec != self.providers.spec_to_route(spec);
            if !is_combo {
                let (name, model) = match crate::providers::ProvidersFile::split_route(spec) {
                    Some(r) => r,
                    None => {
                        return Err(ConfigError::Invalid {
                            field: "providers.active",
                            message: format!(
                                "route `{spec}` must be `provider` or `provider/model`"
                            ),
                        });
                    }
                };
                let entry = match self.providers.provider(&name) {
                    Some(e) => e,
                    None => {
                        return Err(ConfigError::Invalid {
                            field: "providers.active",
                            message: format!("no such provider `{name}` in providers.toml"),
                        });
                    }
                };
                if let Some(m) = &model {
                    if !entry.models.is_empty() && !entry.models.iter().any(|x| x.id == *m) {
                        return Err(ConfigError::Invalid {
                            field: "providers.active",
                            message: format!(
                                "provider `{name}` has no model `{m}` (declared: {})",
                                entry
                                    .models
                                    .iter()
                                    .map(|x| x.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        });
                    }
                } else if entry.default_model.is_none() {
                    return Err(ConfigError::Invalid {
                        field: "providers.active",
                        message: format!(
                            "provider `{name}` names no model and has no default_model; use the route `{name}/<model>`"
                        ),
                    });
                }
            }
        }

        // ponytail: not checking executable bit — OS will error at spawn with clear message.

        // data_dir: if exists, must be a dir; if not, parent must be creatable.
        if self.data_dir.exists() {
            if !self.data_dir.is_dir() {
                return Err(ConfigError::Invalid {
                    field: "data_dir",
                    message: format!(
                        "'{}' exists but is not a directory",
                        self.data_dir.display()
                    ),
                });
            }
        } else {
            // Check that we can create it (parent exists and is writable).
            let parent = self.data_dir.parent();
            if let Some(p) = parent
                && !p.exists()
            {
                return Err(ConfigError::Invalid {
                    field: "data_dir",
                    message: format!("parent directory '{}' does not exist", p.display()),
                });
            }
            // root with overlayfs etc. OS will give a clear error at write time.
        }

        // memory_dir: when enabled, the nearest existing ancestor must be a directory.
        // (When disabled, a stale path is allowed.)
        if self.memory_enabled
            && let Some(dir) = self.memory_dir.as_deref()
            && let Some(p) = dir.ancestors().find(|a| a.exists())
            && !p.is_dir()
        {
            return Err(ConfigError::Invalid {
                field: "memory_dir",
                message: format!("'{}' exists but is not a directory", p.display()),
            });
        }

        // mcp servers: a listed server must be startable — an entry with no
        // name or no command can only fail later at spawn time.
        // ponytail: uniqueness inside the same loop — no second pass needed.
        let mut seen = HashSet::new();
        for s in &self.mcp_servers {
            if s.name.trim().is_empty() {
                return Err(ConfigError::Invalid {
                    field: "mcp.servers.name",
                    message: "must not be empty".to_string(),
                });
            }
            // A server is startable if it names a command or a url; an entry
            // with neither can only fail later at spawn/connect time.
            let has_command = s
                .command
                .as_deref()
                .map(str::trim)
                .is_some_and(|c| !c.is_empty());
            if !has_command && s.url.as_deref().map(str::trim).is_none_or(str::is_empty) {
                return Err(ConfigError::Invalid {
                    field: "mcp.servers",
                    message: format!("server '{}' has neither command nor url", s.name),
                });
            }
            // Ambiguous transport: the mcp crate prefers `url` (streamable
            // HTTP) and silently ignores `command` when both are set. Warn —
            // don't reject — so a legacy config keeps loading while the
            // unused `command` stops being a silent surprise.
            let trimmed_url = s.url.as_deref().map(str::trim);
            let has_url = trimmed_url.is_some_and(|u| !u.is_empty());
            if has_command && has_url {
                eprintln!(
                    "warn: mcp server `{}` has both `url` and `command`; the HTTP transport (url) takes precedence, `command` is ignored",
                    s.name
                );
            }
            // A non-http(s) scheme cannot be dialed by the HTTP transport;
            // warn here so the eventual connect failure points back at the
            // config line. Still not a rejection: validate stays `Ok`.
            if let Some(url) = trimmed_url.filter(|u| !u.is_empty())
                && !url.starts_with("http://")
                && !url.starts_with("https://")
            {
                eprintln!(
                    "warn: mcp server `{}` url `{url}` does not start with http(s)://; connection will likely fail",
                    s.name
                );
            }
            // Checked last so a duplicate error only names an otherwise-valid server.
            if !seen.insert(s.name.clone()) {
                return Err(ConfigError::Invalid {
                    field: "mcp.servers.name",
                    message: format!("duplicate server name '{}'", s.name),
                });
            }
        }

        // subagent providers: a listed provider must be startable, and must
        // not shadow `fork` — the built-in in-process provider. A user config
        // that redefines `fork` would silently route runs to the wrong place.
        for (i, p) in self.subagent_providers.iter().enumerate() {
            if p.name.trim().is_empty() {
                return Err(ConfigError::Invalid {
                    field: "subagent.providers",
                    message: format!("entry {i}: name must not be empty"),
                });
            }
            if p.name == "fork" {
                return Err(ConfigError::Invalid {
                    field: "subagent.providers",
                    message: format!(
                        "entry {i}: name 'fork' is the built-in in-process provider and cannot be overridden"
                    ),
                });
            }
            if p.command.trim().is_empty() {
                return Err(ConfigError::Invalid {
                    field: "subagent.providers",
                    message: format!("entry {i}: an out-of-process provider requires a command"),
                });
            }
        }

        // cwd: when it exists it must be a directory — instruction discovery
        // walks its ancestor chain, and a file would never hold AGENTS.md.
        if self.cwd.exists() && !self.cwd.is_dir() {
            return Err(ConfigError::Invalid {
                field: "cwd",
                message: format!("'{}' exists but is not a directory", self.cwd.display()),
            });
        }
        // max_turns: 0 would end every run before its first LLM round-trip.
        if let Some(n) = self.max_turns
            && n == 0
        {
            return Err(ConfigError::Invalid {
                field: "max_turns",
                message: "must be at least 1".to_string(),
            });
        }

        // providers: structural rules for the `providers.toml` table.
        // Capability values are validated here at the boundary so every
        // consumer (orbit, web status line, attachment gating) sees a
        // normalised, checked value.
        for (name, entry) in &self.providers.providers {
            let mut seen_models = std::collections::HashSet::new();
            for m in &entry.models {
                if m.id.trim().is_empty() {
                    return Err(ConfigError::Invalid {
                        field: "providers.models",
                        message: format!("provider `{name}`: model id must not be empty"),
                    });
                }
                if !seen_models.insert(m.id.clone()) {
                    return Err(ConfigError::Invalid {
                        field: "providers.models",
                        message: format!("provider `{name}`: duplicate model id `{}`", m.id),
                    });
                }
                if m.context_window.is_some_and(|w| w == 0) {
                    return Err(ConfigError::Invalid {
                        field: "providers.models.context_window",
                        message: format!(
                            "provider `{name}` model `{}`: context_window must be a positive integer",
                            m.id
                        ),
                    });
                }
                for modality in &m.input {
                    if crate::providers::ModelInput::parse(modality).is_none() {
                        return Err(ConfigError::Invalid {
                            field: "providers.models.input",
                            message: format!(
                                "provider `{name}` model `{}`: unknown input modality `{modality}` (expected `text` or `image`)",
                                m.id
                            ),
                        });
                    }
                }
            }
            for hop in &entry.fallbacks {
                if crate::providers::ProvidersFile::split_route(hop).is_none() {
                    return Err(ConfigError::Invalid {
                        field: "providers.fallbacks",
                        message: format!(
                            "provider `{name}`: fallback route `{hop}` must be `provider` or `provider/model`"
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    /// Resolve the absolute path to the session database file.
    ///
    /// Priority:
    /// 1. `KYMIDO_SESSION_DB` env var (verbatim).
    /// 2. Platform-specific config directory + `kymido/sessions.db`.
    ///
    /// Returns `ConfigError::Invalid` when the platform-default lookup needs
    /// an environment variable that is missing (e.g. `XDG_CONFIG_HOME` set
    /// to empty, or `HOME`/`APPDATA` unset on Unix/macOS/Windows).
    pub fn session_db_path(&self) -> Result<PathBuf, ConfigError> {
        match env::var_os("KYMIDO_SESSION_DB") {
            Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
            _ => Ok(platform_config_dir()?.join("kymido").join("sessions.db")),
        }
    }

    /// Resolve the absolute path to the daemon Unix-domain / named-pipe socket.
    ///
    /// Priority:
    /// 1. `KYMIDO_DAEMON_SOCKET` env var (verbatim).
    /// 2. Platform-specific config directory + `kymido/daemon.sock`.
    ///
    /// Same error semantics as [`Self::session_db_path`].
    pub fn daemon_socket_path(&self) -> Result<PathBuf, ConfigError> {
        match env::var_os("KYMIDO_DAEMON_SOCKET") {
            Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
            _ => Ok(platform_config_dir()?.join("kymido").join("daemon.sock")),
        }
    }
}

/// Resolve the platform-specific user config directory root.
///
/// Unix/Linux: `$XDG_CONFIG_HOME` if non-empty, else `$HOME/.config`.
/// macOS:     `$HOME/Library/Application Support`.
/// Windows:   `%APPDATA%` (verbatim).
///
/// Returns `ConfigError::Invalid` if the underlying env var is missing or
/// empty (no silent fallback to the current working directory).
fn platform_config_dir() -> Result<PathBuf, ConfigError> {
    #[cfg(target_family = "unix")]
    {
        if let Some(v) = env::var_os("XDG_CONFIG_HOME")
            && !v.is_empty()
        {
            return Ok(PathBuf::from(v));
        }
        match env::var_os("HOME") {
            Some(v) if !v.is_empty() => Ok(PathBuf::from(v).join(".config")),
            _ => Err(ConfigError::Invalid {
                field: "platform_config_dir",
                message: "neither XDG_CONFIG_HOME nor HOME is set".to_string(),
            }),
        }
    }
    #[cfg(target_family = "windows")]
    {
        match env::var_os("APPDATA") {
            Some(v) if !v.is_empty() => Ok(PathBuf::from(v)),
            _ => Err(ConfigError::Invalid {
                field: "platform_config_dir",
                message: "APPDATA is not set".to_string(),
            }),
        }
    }
    #[cfg(not(any(target_family = "unix", target_family = "windows")))]
    {
        // ponytail: no spec'd target here; surface a clear error rather than silently fall back.
        Err(ConfigError::Invalid {
            field: "platform_config_dir",
            message: "unsupported target family for platform config directory".to_string(),
        })
    }
}
