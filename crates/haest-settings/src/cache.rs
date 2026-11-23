//! [`CacheSettings`], the `[cache]` setting: where each request's prompt-cache
//! breakpoints go, whether an idle conversation's cache is kept warm, and
//! whether it is compacted before it expires; for every model, or per model.
//! It is also the [`CachePolicy`] a runtime built from settings runs.

mod serde_impls;
#[cfg(test)]
mod tests;

use std::{collections::BTreeMap, time::Duration};

use haest_core::{
    __private::name_matches,
    cache::{CachePlan, CachePolicy, CacheTtl, IdleAction, IdleState, RequestShape, ping_pays},
    compaction::CompactionOptions,
};
use serde::{Deserialize, Serialize};

haest_core::word_enum! {
    /// Whether a block carries a prompt-cache breakpoint, and how long the
    /// cache it writes lives: `off`, `5m` or `1h` ([`CacheTtl`]).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum BreakpointTtl ("cache breakpoint") {
        /// No breakpoint.
        Off = "off",
        /// Five minutes.
        #[default]
        FiveMinutes = "5m",
        /// An hour.
        OneHour = "1h",
    }
}

impl BreakpointTtl {
    /// Its TTL; None for `off`.
    pub fn ttl(self) -> Option<CacheTtl> {
        match self {
            BreakpointTtl::Off => None,
            BreakpointTtl::FiveMinutes => Some(CacheTtl::FiveMinutes),
            BreakpointTtl::OneHour => Some(CacheTtl::OneHour),
        }
    }
}

/// The `[cache]` setting: what haest's own [`CachePolicy`] does with the
/// provider's prompt cache. The default is what haest always did: a
/// breakpoint on the system prompt, the tools and the newest message, each
/// for five minutes; no keep-warm, no compaction while idle.
///
/// ```toml
/// [cache]
/// system = "1h"            # the system prompt: off, 5m or 1h
/// tools = "1h"             # the last tool definition
/// history = "5m"           # the newest message: the rolling breakpoint
/// keep_warm = true         # ping an idle conversation while it pays
/// keep_warm_for = 120      # minutes, from the last turn
/// compact_before_expire = { min_context = "60%", instructions = "Keep the open TODOs." }
///
/// [cache.models."openrouter:google/*"]
/// keep_warm = false
/// ```
///
/// haest checks every plan before it is sent (haest-core's
/// [`CachePlan::checked`]): `1h` on a model without an hour-long cache goes
/// at five minutes, and the human is told.
///
/// ```
/// use haest_core::cache::{CachePlan, CachePolicy, CacheTtl, RequestShape};
/// use haest_settings::{BreakpointTtl, CacheSettings};
///
/// let cache = CacheSettings {
///     system: BreakpointTtl::OneHour,
///     tools: BreakpointTtl::Off,
///     ..CacheSettings::default()
/// };
/// let plan = cache.plan(&RequestShape::new("anthropic:claude-opus-5-5", &[], "Be brief.", &[]));
/// assert_eq!(plan, CachePlan::none().with_system(CacheTtl::OneHour).with_history(CacheTtl::FiveMinutes));
/// ```
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheSettings {
    /// The system prompt's breakpoint. Default `5m`.
    pub system: BreakpointTtl,
    /// The last tool definition's breakpoint. Default `5m`.
    pub tools: BreakpointTtl,
    /// The newest message's breakpoint, which the next request reads the
    /// whole conversation before it from. Default `5m`.
    pub history: BreakpointTtl,
    /// Keep an idle conversation's cache warm: a pre-warm a little before
    /// it expires, while the model's prices say the pings cost less than
    /// writing the cache again (haest-core's [`ping_pays`]). A ping that
    /// wrote instead of read ends it, and the human is told. Default off.
    pub keep_warm: bool,
    /// How long after the last turn keep-warm goes on. Spelled in minutes,
    /// like every time setting. Default an hour.
    #[serde(with = "serde_impls::minutes")]
    pub keep_warm_for: Duration,
    /// Compact an idle conversation while its cache is still warm, so the
    /// next turn writes a small prefix instead of the whole one. Default off.
    pub compact_before_expire: CompactBeforeExpire,
    /// Overrides for the models whose spec matches the key (exact, or
    /// `prefix*`), the most specific last: an exact spec over any pattern,
    /// a longer prefix over a shorter one. Each sets only what it names.
    pub models: BTreeMap<String, CacheOverride>,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            system: BreakpointTtl::FiveMinutes,
            tools: BreakpointTtl::FiveMinutes,
            history: BreakpointTtl::FiveMinutes,
            keep_warm: false,
            keep_warm_for: Duration::from_secs(3_600),
            compact_before_expire: CompactBeforeExpire::Off,
            models: BTreeMap::new(),
        }
    }
}

/// What a `[cache.models."<pattern>"]` table sets: any key of `[cache]` but
/// `models`, each None where it leaves the one before it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheOverride {
    /// [`CacheSettings::system`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<BreakpointTtl>,
    /// [`CacheSettings::tools`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<BreakpointTtl>,
    /// [`CacheSettings::history`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<BreakpointTtl>,
    /// [`CacheSettings::keep_warm`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<bool>,
    /// [`CacheSettings::keep_warm_for`], in minutes.
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "serde_impls::minutes::option"
    )]
    pub keep_warm_for: Option<Duration>,
    /// [`CacheSettings::compact_before_expire`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_before_expire: Option<CompactBeforeExpire>,
}

/// `compact_before_expire`: `"off"`, or the conditions under which an idle
/// conversation is compacted before its cache expires.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub enum CompactBeforeExpire {
    /// Never.
    #[default]
    Off,
    /// When the conversation holds at least `min_context`.
    When {
        /// How big the conversation must be: the tokens its last request
        /// and answer took.
        min_context: MinContext,
        /// What the summary should keep or leave out, after haest's own
        /// instruction ([`CompactionOptions::instructions`]).
        instructions: Option<String>,
    },
}

/// A size of conversation: tokens, or a share of the model's context window.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum MinContext {
    /// This many tokens (`120000`).
    Tokens(u64),
    /// This percentage of the context window (`"60%"`); not reached while
    /// the window is unknown.
    Percent(f64),
}

impl MinContext {
    /// Whether `tokens` reach it, in a window of `window` tokens.
    pub fn reached(self, tokens: u64, window: Option<u64>) -> bool {
        match self {
            MinContext::Tokens(min) => tokens >= min,
            MinContext::Percent(p) => window.is_some_and(|w| tokens as f64 >= w as f64 * p / 100.0),
        }
    }
}

impl CacheSettings {
    /// What applies to `model_spec`: these settings with every override
    /// whose pattern matches it laid on, the most specific last; `models`
    /// left empty.
    ///
    /// ```
    /// use haest_settings::{BreakpointTtl, CacheOverride, CacheSettings};
    ///
    /// let mut cache = CacheSettings::default();
    /// let mut google = CacheOverride::default();
    /// google.system = Some(BreakpointTtl::Off);
    /// cache.models.insert("openrouter:google/*".into(), google);
    /// assert_eq!(cache.for_model("openrouter:google/gemini-4-argon").system, BreakpointTtl::Off);
    /// assert_eq!(cache.for_model("anthropic:claude-opus-5-5").system, BreakpointTtl::FiveMinutes);
    /// ```
    #[must_use]
    pub fn for_model(&self, model_spec: &str) -> CacheSettings {
        let mut matching: Vec<(&String, &CacheOverride)> = (self.models.iter())
            .filter(|(pattern, _)| name_matches(model_spec, pattern))
            .collect();
        // Exact after every pattern; a longer prefix after a shorter one.
        matching.sort_by_key(|(pattern, _)| (!pattern.ends_with('*'), pattern.len()));
        let mut out = CacheSettings {
            models: BTreeMap::new(),
            ..self.clone()
        };
        for (_, o) in matching {
            let o = o.clone();
            out.system = o.system.unwrap_or(out.system);
            out.tools = o.tools.unwrap_or(out.tools);
            out.history = o.history.unwrap_or(out.history);
            out.keep_warm = o.keep_warm.unwrap_or(out.keep_warm);
            out.keep_warm_for = o.keep_warm_for.unwrap_or(out.keep_warm_for);
            if let Some(compact) = o.compact_before_expire {
                out.compact_before_expire = compact;
            }
        }
        out
    }
}

/// The settings' plan for each request, and at an idle tick: compact when
/// `compact_before_expire` is on, the conversation holds `min_context` and
/// compacting pays ([`IdleState::compaction_pays`]); else ping when
/// `keep_warm` is on, the human has been away for less than
/// `keep_warm_for`, and [`ping_pays`]; else nothing. haest's own checks
/// (the cache still warm, a compaction that pays) come after.
impl CachePolicy for CacheSettings {
    fn plan(&self, request: &RequestShape<'_>) -> CachePlan {
        let s = self.for_model(request.model_spec);
        let mut plan = CachePlan::none();
        plan.system = s.system.ttl();
        plan.tools = s.tools.ttl();
        plan.history = s.history.ttl();
        plan
    }

    fn on_idle(&self, idle: &IdleState<'_>) -> IdleAction {
        let s = self.for_model(idle.model_spec);
        if let CompactBeforeExpire::When {
            min_context,
            instructions,
        } = s.compact_before_expire
            && min_context.reached(idle.prefix_tokens, idle.context_window)
        {
            let mut options = CompactionOptions::new(idle.context_window.unwrap_or(0));
            options.instructions = instructions;
            if idle.compaction_pays(&options) {
                return IdleAction::Compact(options);
            }
        }
        if s.keep_warm
            && idle.idle < s.keep_warm_for
            && ping_pays(idle.pricing, idle.ttl, idle.idle)
        {
            return IdleAction::Ping;
        }
        IdleAction::Nothing
    }
}
