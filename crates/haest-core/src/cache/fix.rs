//! [`PlanFix`]: what [`CachePlan::checked`](super::CachePlan::checked)
//! changed in a plan and why, by [`Breakpoint`], and the limits it checks
//! against.

use std::fmt;

/// The most breakpoints one request takes (Anthropic's rule; OpenRouter
/// passes it on).
pub const MAX_BREAKPOINTS: usize = 4;

/// The tokens a request with tools may carry beyond what haest sends: the
/// instructions a provider adds for its tools. Anthropic adds 286 to 804,
/// by model and tool choice; this rounds up, so the minimum check
/// ([`PlanFix::BelowMinimum`]) never takes a request with tools for
/// shorter than it is.
pub const TOOL_PROMPT_ALLOWANCE: u64 = 1_024;

/// A place in a request a breakpoint marks, in the order the provider
/// caches the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Breakpoint {
    /// The last tool definition.
    Tools,
    /// The system prompt.
    System,
    /// The message at this index of the request's messages.
    Message(usize),
    /// The newest message.
    History,
}

impl fmt::Display for Breakpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Breakpoint::Tools => f.write_str("the tools"),
            Breakpoint::System => f.write_str("the system prompt"),
            Breakpoint::Message(i) => write!(f, "message {i}"),
            Breakpoint::History => f.write_str("the newest message"),
        }
    }
}

/// What [`CachePlan::checked`](super::CachePlan::checked) changed in a
/// plan, and why. Its `Display` is the note a human or a log reads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlanFix {
    /// A chosen message past the request's end: dropped.
    NoSuchMessage {
        /// The index the plan named.
        index: usize,
        /// How many messages the request has.
        messages: usize,
    },
    /// The model keeps no cache entry for an hour: these went at five minutes.
    NoHour {
        /// The breakpoints lowered.
        at: Vec<Breakpoint>,
    },
    /// The request up to the breakpoint is surely shorter than the model
    /// caches: dropped, since it would cache nothing.
    ///
    /// "Surely": haest has no tokenizer, so it bounds a prefix from above,
    /// taking a token as at least one byte of its text (true of a byte-level
    /// tokenizer), counting the JSON a message is sent as, adding
    /// [`TOOL_PROMPT_ALLOWANCE`] when there are tools, and never dropping a
    /// breakpoint at or after a message with media. A breakpoint that might
    /// cache is kept: the provider ignores one that does not.
    BelowMinimum {
        /// The breakpoint dropped.
        at: Breakpoint,
        /// The most tokens the request up to it can be.
        at_most: u64,
        /// The model's minimum.
        minimum: u64,
    },
    /// More than [`MAX_BREAKPOINTS`]: the oldest chosen messages dropped.
    TooMany {
        /// The breakpoints dropped.
        dropped: Vec<Breakpoint>,
    },
    /// A shorter TTL before a longer one, which the provider refuses: these
    /// were raised to the longest after them. That costs nothing more, as
    /// the provider bills everything before an hour-long breakpoint as an
    /// hour-long write anyway.
    Raised {
        /// The breakpoints raised.
        at: Vec<Breakpoint>,
    },
}

impl PlanFix {
    /// Whether the human should hear it: the plan asked for what the model
    /// or the provider cannot do. The other fixes go to the diagnostic log.
    pub fn is_warning(&self) -> bool {
        matches!(self, PlanFix::NoHour { .. } | PlanFix::TooMany { .. })
    }
}

/// `places`, joined with commas and a final "and".
fn listed(places: &[Breakpoint]) -> String {
    let words: Vec<String> = places.iter().map(ToString::to_string).collect();
    match words.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => words.join(""),
    }
}

impl fmt::Display for PlanFix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanFix::NoSuchMessage { index, messages } => write!(
                f,
                "the cache breakpoint on message {index} was dropped: the request has {messages} messages"
            ),
            PlanFix::NoHour { at } => write!(
                f,
                "the model keeps no cache entry for an hour: {} cached for five minutes",
                listed(at)
            ),
            PlanFix::BelowMinimum {
                at,
                at_most,
                minimum,
            } => write!(
                f,
                "the cache breakpoint on {at} was dropped: the request up to it (at most {at_most} tokens) is shorter than the {minimum} the model caches"
            ),
            PlanFix::TooMany { dropped } => write!(
                f,
                "a request takes at most {MAX_BREAKPOINTS} cache breakpoints: the {} on {} dropped",
                if dropped.len() == 1 { "one" } else { "ones" },
                listed(dropped)
            ),
            PlanFix::Raised { at } => write!(
                f,
                "{} cached as long as a later breakpoint: a longer TTL must come first",
                listed(at)
            ),
        }
    }
}
