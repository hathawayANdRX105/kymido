//! `providers.toml` — provider credentials + per-model capability wire format.
//!
//! A sibling file next to `config.toml` (same two-scope shadowing: the
//! workspace `.kymido/providers.toml` shadows the user-level file whole,
//! the same way a project `.env` beats `~/.env`). It is the only place
//! provider credentials live: the flat `[llm]` section was removed with
//! the cutover (loading a config that still carries one is a hard error
//! with a migration hint, not a silent alias).
//!
//! Shape (TOML native named tables):
//!
//! ```toml
//! active = "fast"
//!
//! [combos]
//! fast = "deepseek/deepseek-chat"
//!
//! [providers.deepseek]
//! base_url = "https://api.deepseek.com"
//! api_key_env = "DEEPSEEK_API_KEY"
//! default_model = "deepseek-chat"
//! fallbacks = ["openai/gpt-4o-mini"]
//!
//! [[providers.deepseek.models]]
//! id = "deepseek-chat"
//! context_window = 65536
//! input = ["text"]
//! ```

use serde::Deserialize;
use std::collections::HashMap;

/// Declared input modalities. Values are validated at the config boundary
/// (unknown strings are a load error); lower layers carry the plain
/// strings and check for the image modality by name.
pub const MODALITY_TEXT: &str = "text";
pub const MODALITY_IMAGE: &str = "image";

/// One input modality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInput {
    /// Plain text in/out.
    Text,
    /// Image blocks ride as OpenAI multimodal `image_url` parts.
    Image,
}

impl ModelInput {
    /// Parse a declared modality string; `None` when unknown (the caller
    /// turns it into a load error naming the offending entry).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            MODALITY_TEXT => Some(Self::Text),
            MODALITY_IMAGE => Some(Self::Image),
            _ => None,
        }
    }
}

/// `providers.toml` root: the active spec, named model combos, and every
/// `[providers.<name>]` entry. All fields optional — an empty file is a
/// valid (no-credential) machine, same contract as `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProvidersFile {
    /// Active spec: a combo name, `"provider"`, or `"provider/model"`;
    /// absent = no direct LLM credential (the daemon runs omp-compat mode,
    /// the historic path).
    #[serde(default)]
    pub active: Option<String>,
    /// `[combos]`: named model combos (`"name" -> "provider/model"`).
    /// One hop only: a value must be a literal route, never another combo
    /// name (validate rejects multi-hop indirection and bare-provider
    /// targets). `active` and `fallbacks` entries may reference a combo.
    #[serde(default)]
    pub combos: HashMap<String, String>,
    /// `[providers.<name>]` entries, keyed by provider name.
    #[serde(default)]
    pub providers: HashMap<String, ProviderEntry>,
}

/// Whether a provider entry can actually produce a request. Decided locally,
/// with no network call: a probe at daemon start would make startup depend
/// on a third-party endpoint answering, which is exactly the failure a
/// config check must not introduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderStatus {
    /// `base_url` present, a key is resolvable, and a model is named
    /// (`default_model` or a single declared entry).
    Ready,
    /// No usable key: neither `api_key` nor the `api_key_env` variable.
    MissingKey,
    /// `base_url` or the resolvable model is missing.
    Incomplete,
}

impl ProviderStatus {
    /// True only for [`ProviderStatus::Ready`].
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

impl ProviderEntry {
    /// The key to use: `api_key_env` when set (and set in the environment),
    /// else the inline `api_key`.
    pub fn resolve_api_key(&self) -> Option<String> {
        self.api_key_env
            .as_ref()
            .and_then(|name| std::env::var(name).ok())
            .or_else(|| self.api_key.clone())
    }

    /// Can this entry serve a request right now?
    pub fn status(&self) -> ProviderStatus {
        if self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .is_none()
            || self
                .default_model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .is_none()
        {
            return ProviderStatus::Incomplete;
        }
        match self.resolve_api_key() {
            Some(k) if !k.trim().is_empty() => ProviderStatus::Ready,
            _ => ProviderStatus::MissingKey,
        }
    }
}

/// One `[providers.<name>]` entry: a provider's credential plus its
/// waterfall and per-model capability table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProviderEntry {
    /// OpenAI-compatible endpoint. No `/v1` suffix (the call site appends
    /// it). Empty/absent = this entry cannot serve a request.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Inline API key. Prefer `api_key_env` in a shared file.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Environment variable to read the key from (literal name, read via
    /// `env::var` — no prefix or interpolation). Wins over `api_key`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Model used when the active route names the provider without a model.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Provider-level max tokens; a model entry's own `max_tokens` wins.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Waterfall routes (`"other/model"`, listed order) tried after this
    /// provider fails before emitting content. Credentials inherit the
    /// *target* provider's row; capabilities follow the target model entry.
    #[serde(default)]
    pub fallbacks: Vec<String>,
    /// `[[providers.<name>.models]]` capability entries.
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

/// One `[[providers.<name>.models]]` entry: per-model capability data.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelEntry {
    /// Model id sent to the endpoint. Must be unique inside the provider.
    pub id: String,
    /// Context window in tokens; drives the compaction char budget
    /// (`total_chars = window × 4`). Must be positive when present;
    /// absent = the engine's built-in budget (zero behaviour change).
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Declared input modalities. Absent = `["text"]` (text-only is the
    /// conservative default: image attachments stay gated off).
    #[serde(default = "default_input")]
    pub input: Vec<String>,
    /// Per-model max tokens; wins over the provider-level value.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

fn default_input() -> Vec<String> {
    vec![MODALITY_TEXT.to_string()]
}

impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            context_window: None,
            input: default_input(),
            max_tokens: None,
        }
    }
}

/// The credentials a resolved route (or a fallback hop) actually dials.
/// Every field present: a missing piece means the caller falls back to
/// "no direct credential" rather than guessing.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLlm {
    /// Base URL without the `/v1` suffix.
    pub base_url: String,
    pub api_key: String,
    /// The concrete model id sent to the endpoint.
    pub model: String,
    pub max_tokens: Option<u32>,
    /// Declared context window (tokens); `None` = engine default budget.
    pub context_window: Option<u32>,
    /// Declared input modalities (always non-empty; absent data = text).
    pub input: Vec<String>,
}

impl ResolvedLlm {
    /// True when image blocks are an advertised input modality.
    pub fn image_input(&self) -> bool {
        self.input.iter().any(|m| m == MODALITY_IMAGE)
    }
}

impl ProvidersFile {
    /// Parse one active-or-fallback route string: `"provider"` or
    /// `"provider/model"`. Structural failures return `None` for the
    /// callers to shape (a bad `active` becomes a load error; a bad
    /// fallback route a warn-and-skip, B2b1 semantics).
    pub fn split_route(route: &str) -> Option<(String, Option<String>)> {
        let route = route.trim();
        match route.split_once('/') {
            Some((provider, model)) if !provider.trim().is_empty() && !model.trim().is_empty() => {
                Some((provider.trim().to_string(), Some(model.trim().to_string())))
            }
            _ if !route.is_empty() && !route.contains('/') => Some((route.to_string(), None)),
            _ => None,
        }
    }

    /// Look up a provider entry by name.
    pub fn provider(&self, name: &str) -> Option<&ProviderEntry> {
        self.providers.get(name)
    }

    /// Resolve a route against this file: the provider row's credentials
    /// plus the chosen model entry's capability data. `model` absent in the
    /// route = the provider's `default_model`.
    ///
    /// Model resolution rules: when the provider lists `models`, a named
    /// model must be one of them (typos are load errors, the active
    /// credential must never silently pick a different model). When it
    /// lists none, any non-empty model id passes through verbatim —
    /// hand-written adapters forward arbitrary ids to public or private
    /// endpoints, and a shared catalog would break that.
    pub fn resolve_route(&self, route: &str) -> Option<ResolvedLlm> {
        let (provider_name, named_model) = Self::split_route(route)?;
        let provider = self.providers.get(&provider_name)?;
        let model = match &named_model {
            Some(m) => m.clone(),
            None => provider.default_model.clone()?,
        };
        let base_url = match provider.base_url.as_deref() {
            Some(url) if !url.trim().is_empty() => url.to_string(),
            _ => return None,
        };
        // A named model must exist in a declared capability table; an
        // undeclared table keeps arbitrary ids flowing.
        if !provider.models.is_empty() && !provider.models.iter().any(|m| m.id == model) {
            return None;
        }
        let entry = provider.models.iter().find(|m| m.id == model);
        let api_key = provider
            .api_key_env
            .as_ref()
            .and_then(|name| std::env::var(name).ok())
            .or_else(|| provider.api_key.clone())?;
        Some(ResolvedLlm {
            base_url,
            api_key,
            model,
            max_tokens: entry.and_then(|m| m.max_tokens).or(provider.max_tokens),
            context_window: entry.and_then(|m| m.context_window),
            input: entry.map(|m| m.input.clone()).unwrap_or_else(default_input),
        })
    }

    /// The literal route behind a model spec. A combo name is looked up in
    /// `[combos]` (one hop: the value must itself be a literal route, so a
    /// combo can never point at another combo); anything else — a slashed
    /// route or an unknown bare name — passes through verbatim. Slashed
    /// specs are never combo-looked-up: a combo named like a route is simply
    /// unreachable, which is a validate error, not a silent indirection.
    pub fn spec_to_route<'a>(&'a self, spec: &'a str) -> &'a str {
        let spec = spec.trim();
        if spec.contains('/') {
            return spec;
        }
        self.combos.get(spec).map(|t| t.as_str()).unwrap_or(spec)
    }

    /// Resolve a model spec — a combo name, `"provider"`, or
    /// `"provider/model"` — to the dialed credentials. The combo hop is
    /// single-shot: `spec_to_route` substitutes the target once and hands
    /// the literal route to [`Self::resolve_route`].
    pub fn resolve_spec(&self, spec: &str) -> Option<ResolvedLlm> {
        self.resolve_route(self.spec_to_route(spec))
    }
}
