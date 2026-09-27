//! Waterfall LLM fallback: try a primary OpenAI-compatible provider, and
//! only when a whole call to it fails *cleanly* (terminal `Error`, no
//! delta/tool-call emitted to the consumer) drop to the next configured
//! provider. Per-provider retries (exponential backoff) still run inside
//! each attempt; the waterfall is the layer above them.
//!
//! A provider's intermediate terminal `Error` is swallowed, never forwarded:
//! the agent loop's `stream_failed` latch ([`super::run_agent_streaming`]) is
//! one-way, so a forwarded Error would sink the whole turn even when a later
//! provider succeeds. The consumer sees an `Error` only when content already
//! leaked (no replay possible) or after every provider failed.
//!
//! Placement note: the [`LlmBackend`] trait lives in this crate (orbit),
//! and adaptor is orbit's dependency — so the waterfall *runtime* must
//! live here, not in `llm::fallback`. `llm::openai::stream_cb_with_policy`
//! supplies the per-provider call; this module sequences the providers.

use std::sync::atomic::AtomicBool;

use parking_lot::Mutex;

use llm::{Context, Model, StreamEvent, ToolDef, openai::RetryPolicy};

use super::LlmBackend;

/// Terminal shape of one per-provider call: `stream_cb_with_policy` ends
/// every attempt with exactly one `Done` or one `Error` (both after any
/// deltas/tool-calls), so "the last event seen" decides the waterfall move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    None,
    Done,
    Error,
}

/// One OpenAI-compatible LLM provider endpoint. Constructed programmatically
/// (the daemon maps `config::LlmFallbackConfig` onto this in
/// `orbit_setup`); there is no serde path into it, so it derives only what
/// the runtime needs.
#[derive(Debug, Clone)]
pub struct LlmProvider {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
}

impl LlmProvider {
    fn to_model(&self) -> Model {
        Model {
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            base_url: self.base_url.clone(),
            max_tokens: self.max_tokens,
        }
    }
}

/// Waterfall runtime over [`LlmProvider`]s: `primary` first, then
/// `fallbacks` in listed order. A provider only yields to the next one
/// when its whole call fails without leaking any content to the consumer
/// (no `TextDelta`/`ToolCall` emitted); a partially-emitted turn is never
/// replayed on another provider, and aborts (consumer intent) stop
/// immediately.
#[derive(Debug)]
pub struct WaterfallLlm {
    pub primary: LlmProvider,
    pub fallbacks: Vec<LlmProvider>,
    pub retry: RetryPolicy,
    /// The model of the most recent provider that served a round cleanly.
    /// Lets the status line report the *active* provider rather than the
    /// configured primary once a fallback has taken over. `None` until the
    /// first successful (or cleanly aborted) round.
    last_model: Mutex<Option<String>>,
}

impl Clone for WaterfallLlm {
    /// The per-instance winner is dropped on clone: a new clone starts with
    /// no active model yet. The provider list is what matters.
    fn clone(&self) -> Self {
        Self {
            primary: self.primary.clone(),
            fallbacks: self.fallbacks.clone(),
            retry: self.retry.clone(),
            last_model: Mutex::new(None),
        }
    }
}

impl WaterfallLlm {
    pub fn new(primary: LlmProvider, fallbacks: Vec<LlmProvider>) -> Self {
        Self {
            primary,
            fallbacks,
            retry: RetryPolicy::default(),
            last_model: Mutex::new(None),
        }
    }

    /// Build with an explicit per-provider retry policy; the winner record
    /// starts empty.
    pub fn with_retry(
        primary: LlmProvider,
        fallbacks: Vec<LlmProvider>,
        retry: RetryPolicy,
    ) -> Self {
        Self {
            primary,
            fallbacks,
            retry,
            last_model: Mutex::new(None),
        }
    }

    /// Single-provider runtime, no fallbacks — equivalent to the historic
    /// `HttpLlm` behaviour (default retry policy).
    pub fn solo(primary: LlmProvider) -> Self {
        Self::new(primary, Vec::new())
    }

    /// Every provider in waterfall order: primary first, then fallbacks.
    fn providers(&self) -> Vec<&LlmProvider> {
        let mut out = vec![&self.primary];
        out.extend(self.fallbacks.iter());
        out
    }

    /// One provider's call plus the waterfall decision on it.
    ///
    /// Returns `(finished, last_error)`: `finished` is `true` when the round
    /// is over (success, a clean abort, or a failure that already leaked
    /// content — the error was just emitted and must not be retried
    /// elsewhere) and `false` when the provider failed *before emitting
    /// anything* and the next provider should take over. `last_error` is the
    /// terminal `Error` text when the call failed, so the waterfall can
    /// aggregate per-provider failures on total exhaustion.
    ///
    /// Forwarding rule: every event is passed to the consumer in order,
    /// *except* a terminal `Error` from a call that leaked no content. That
    /// Error is swallowed — the agent loop's `stream_failed` latch is
    /// one-way, so forwarding it would fail the whole turn even when the
    /// next provider succeeds. When content already leaked, the Error must
    /// be forwarded (no replay is possible and the consumer needs the
    /// failure). The per-provider call ends with exactly one terminal event
    /// (Done or Error), so tracking the last event is enough.
    fn attempt_provider(
        provider: &LlmProvider,
        context: &Context,
        tools: &[ToolDef],
        signal: &AtomicBool,
        policy: RetryPolicy,
        emit: &mut dyn FnMut(&StreamEvent),
    ) -> Result<bool, String> {
        // Observe every event the per-provider call hands out: record
        // whether content leaked, whether the round ended in a clean abort,
        // and what the terminal event was — while forwarding every event
        // to the consumer via `emit`, in order, except the swallow case
        // below.
        let mut leaked_content = false;
        let mut terminal = Terminal::None;
        // The last terminal error text, so a 401/403 on the last provider
        // survives the "all providers exhausted" summary.
        let mut last_error: Option<String> = None;
        llm::openai::stream_cb_with_policy(
            &provider.to_model(),
            context,
            tools,
            signal,
            &mut |ev| {
                match ev {
                    StreamEvent::TextDelta(_)
                    | StreamEvent::ToolCall(_)
                    | StreamEvent::ReasoningDelta(_) => {
                        leaked_content = true;
                    }
                    StreamEvent::Done { .. } => {
                        terminal = Terminal::Done;
                    }
                    StreamEvent::Error(msg) => {
                        terminal = Terminal::Error;
                        last_error = Some(msg.clone());
                    }
                }
                // A terminal Error with nothing leaked is withheld: it only
                // means "this provider failed, try the next one", not "the
                // turn failed". Forward everything else verbatim.
                if terminal != Terminal::Error || leaked_content {
                    emit(ev);
                }
            },
            policy,
        );
        if terminal != Terminal::Error {
            // Clean `Done`: success (an abort is consumer intent, also done).
            // `true` = served the round cleanly (success or a clean abort),
            // so this provider is the *active* model for the status line.
            return Ok(true);
        }
        if leaked_content {
            // Content leaked before the error: the failure was just emitted
            // and must not be replayed elsewhere. The waterfall stops here.
            // `false` = content leaked before the error. The failure was
            // emitted to the consumer and must not be replayed on the next
            // provider, so the round is over — but this provider did *not*
            // serve the turn cleanly, hence no `last_model` write.
            return Ok(false);
        }
        // Error, no content leaked, round ended: the next provider may take
        // over — the caller decides (continue or final error).
        Err(last_error.unwrap_or_else(|| "unknown error".into()))
    }

    /// Run the waterfall. After all providers fail, emit one terminal
    /// `Error` that aggregates every provider's own failure text (jcode's
    /// fallback-aggregation shape); after any provider succeeds the round
    /// stops and the winner is remembered for the status line.
    fn run(
        &self,
        context: &Context,
        tools: &[ToolDef],
        signal: &AtomicBool,
        emit: &mut dyn FnMut(&StreamEvent),
    ) {
        let providers = self.providers();
        let last = providers.len() - 1;
        for (i, provider) in providers.iter().enumerate() {
            match Self::attempt_provider(provider, context, tools, signal, self.retry, emit) {
                // Round is over. A clean round (success or consumer abort)
                // means this provider actually served the turn, so it — not
                // the configured primary — is what the status line reports.
                Ok(clean) => {
                    if clean {
                        *self.last_model.lock() = Some(provider.model.clone());
                    }
                    return;
                }
                Err(last_err) => {
                    // This provider failed before leaking anything. The next
                    // provider may still take over — only the last one's
                    // failure is terminal, and it carries the original error
                    // so a 401/403 survives the fallback chain.
                    if i < last {
                        continue;
                    }
                    emit(&StreamEvent::Error(format!(
                        "llm provider {i} ({}) failed: all providers exhausted (last error: {last_err})",
                        provider.model
                    )));
                    return;
                }
            }
        }
    }
}

impl LlmBackend for WaterfallLlm {
    fn stream_cb(
        &self,
        _model: &Model,
        context: &Context,
        tools: &[ToolDef],
        signal: &AtomicBool,
        emit: &mut dyn FnMut(&StreamEvent),
    ) {
        // The providers carry their own models; the loop's `model` argument
        // is the primary's, used only for host-side metadata.
        self.run(context, tools, signal, emit);
    }

    fn active_model(&self) -> Option<String> {
        self.last_model.lock().clone()
    }
}
