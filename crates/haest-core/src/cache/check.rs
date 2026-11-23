//! [`CachePlan::checked`]: a plan fitted to the provider's rules before it
//! is sent, with the [`PlanFix`]es that say what was changed.

use std::collections::BTreeMap;

use super::{
    Breakpoint, CachePlan, CacheTtl, MAX_BREAKPOINTS, PlanFix, RequestShape, TOOL_PROMPT_ALLOWANCE,
};
use crate::media::messages_for_estimate;

/// The longer of two TTLs.
fn longer(a: CacheTtl, b: CacheTtl) -> CacheTtl {
    if a.lifetime() >= b.lifetime() { a } else { b }
}

impl CachePlan {
    /// This plan fitted to the provider's rules for `request`, and what was
    /// changed. haest sends every plan through it, and tells the human of a
    /// fix that [`is_warning`](PlanFix::is_warning) (the rest go to the
    /// diagnostic log), so a plan never makes a request fail.
    ///
    /// In this order: a chosen message past the end is dropped, two on one
    /// message become one, and one on the newest message joins
    /// [`history`](Self::history) (the longer TTL wins); a model whose
    /// [`CacheFacts`](crate::catalog::CacheFacts) say it keeps no entry an
    /// hour gets five minutes; a breakpoint the request is surely too short
    /// to cache up to is dropped ([`PlanFix::BelowMinimum`] says how
    /// "surely" is bounded); past [`MAX_BREAKPOINTS`] the oldest chosen
    /// messages are dropped; and an earlier breakpoint whose TTL is shorter
    /// than a later one's is raised to it. A model's facts unknown, the
    /// second and third are skipped. A tools breakpoint in a request with no
    /// tools, or a history one in a request with no messages, marks nothing
    /// and is taken out without a fix.
    ///
    /// ```
    /// use haest_core::{
    ///     Message,
    ///     cache::{Breakpoint, CachePlan, CacheTtl, PlanFix, RequestShape},
    ///     catalog::CacheFacts,
    /// };
    ///
    /// let messages = [Message::user("hi")];
    /// let five_minutes_only = CacheFacts::new();
    /// let request = RequestShape::new("openrouter:google/gemini-4-argon", &[], "Be brief.", &messages)
    ///     .with_facts(&five_minutes_only);
    /// let asked = CachePlan::none().with_system(CacheTtl::OneHour).with_history(CacheTtl::FiveMinutes);
    ///
    /// let (sent, fixes) = asked.checked(&request);
    /// assert_eq!(sent.system, Some(CacheTtl::FiveMinutes));
    /// assert_eq!(fixes, [PlanFix::NoHour { at: vec![Breakpoint::System] }]);
    /// assert!(fixes[0].is_warning());
    /// ```
    pub fn checked(mut self, request: &RequestShape<'_>) -> (CachePlan, Vec<PlanFix>) {
        let mut fixes = Vec::new();
        let messages = request.messages.len();
        if request.tools.is_empty() {
            self.tools = None;
        }
        if messages == 0 {
            self.history = None;
        }
        let mut chosen = BTreeMap::new();
        for (index, ttl) in std::mem::take(&mut self.messages) {
            if index >= messages {
                fixes.push(PlanFix::NoSuchMessage { index, messages });
            } else if index + 1 == messages {
                self.history = Some(self.history.map_or(ttl, |h| longer(h, ttl)));
            } else {
                let held = chosen.entry(index).or_insert(ttl);
                *held = longer(*held, ttl);
            }
        }
        let mut places = Places { plan: self, chosen };
        if let Some(facts) = request.facts {
            if !facts.ttl_1h {
                places.no_hour(&mut fixes);
            }
            if let Some(minimum) = facts.min_cacheable_tokens {
                places.drop_below(minimum, request, &mut fixes);
            }
        }
        places.at_most_four(&mut fixes);
        places.longer_first(&mut fixes);
        let mut plan = places.plan;
        plan.messages = places.chosen.into_iter().collect();
        (plan, fixes)
    }
}

/// A plan being checked: its chosen messages kept by index, in order.
struct Places {
    plan: CachePlan,
    chosen: BTreeMap<usize, CacheTtl>,
}

impl Places {
    /// Every breakpoint and its TTL, in the provider's order.
    fn in_order(&self) -> Vec<(Breakpoint, CacheTtl)> {
        let fixed = |at, ttl: Option<CacheTtl>| ttl.map(|ttl| (at, ttl));
        let chosen = self
            .chosen
            .iter()
            .map(|(i, t)| (Breakpoint::Message(*i), *t));
        fixed(Breakpoint::Tools, self.plan.tools)
            .into_iter()
            .chain(fixed(Breakpoint::System, self.plan.system))
            .chain(chosen)
            .chain(fixed(Breakpoint::History, self.plan.history))
            .collect()
    }

    /// The TTL of `at`, to change in place.
    fn ttl_mut(&mut self, at: Breakpoint) -> Option<&mut CacheTtl> {
        match at {
            Breakpoint::Tools => self.plan.tools.as_mut(),
            Breakpoint::System => self.plan.system.as_mut(),
            Breakpoint::Message(i) => self.chosen.get_mut(&i),
            Breakpoint::History => self.plan.history.as_mut(),
        }
    }

    fn remove(&mut self, at: Breakpoint) {
        match at {
            Breakpoint::Tools => self.plan.tools = None,
            Breakpoint::System => self.plan.system = None,
            Breakpoint::Message(i) => {
                self.chosen.remove(&i);
            }
            Breakpoint::History => self.plan.history = None,
        }
    }

    fn no_hour(&mut self, fixes: &mut Vec<PlanFix>) {
        let hours: Vec<Breakpoint> = (self.in_order().into_iter())
            .filter(|(_, ttl)| *ttl == CacheTtl::OneHour)
            .map(|(at, _)| at)
            .collect();
        for at in &hours {
            *self.ttl_mut(*at).expect("a breakpoint in the plan") = CacheTtl::FiveMinutes;
        }
        if !hours.is_empty() {
            fixes.push(PlanFix::NoHour { at: hours });
        }
    }

    /// Drops every breakpoint the request up to it is surely too short for
    /// ([`PlanFix::BelowMinimum`] says how that is bounded). The prefix only
    /// grows in the provider's order, so the first one that may be long
    /// enough ends the walk: a long request costs one look at its tools and
    /// system prompt.
    fn drop_below(&mut self, minimum: u64, request: &RequestShape<'_>, fixes: &mut Vec<PlanFix>) {
        let bytes = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let tools = match request.tools {
            [] => 0,
            tools => serde_json::to_string(tools)
                .map_or(u64::MAX, |json| bytes(json.len()))
                .saturating_add(TOOL_PROMPT_ALLOWANCE),
        };
        let head = [
            (Breakpoint::Tools, tools),
            (Breakpoint::System, bytes(request.system.len())),
        ];
        let last = request.messages.len().checked_sub(1);
        // Lazy: a message past the first prefix that may be long enough is
        // never serialized.
        let messages = request.messages.iter().enumerate().map(|(i, m)| {
            let at = if Some(i) == last {
                Breakpoint::History
            } else {
                Breakpoint::Message(i)
            };
            let (json, media) = messages_for_estimate(std::slice::from_ref(m));
            let most = if media > 0 {
                u64::MAX
            } else {
                bytes(json.len())
            };
            (at, most)
        });
        let mut at_most = 0u64;
        for (at, more) in head.into_iter().chain(messages) {
            at_most = at_most.saturating_add(more);
            if at_most >= minimum {
                return;
            }
            if self.ttl_mut(at).is_some() {
                self.remove(at);
                fixes.push(PlanFix::BelowMinimum {
                    at,
                    at_most,
                    minimum,
                });
            }
        }
    }

    fn at_most_four(&mut self, fixes: &mut Vec<PlanFix>) {
        let mut dropped = Vec::new();
        while self.in_order().len() > MAX_BREAKPOINTS {
            let Some((&oldest, _)) = self.chosen.first_key_value() else {
                break;
            };
            self.chosen.remove(&oldest);
            dropped.push(Breakpoint::Message(oldest));
        }
        if !dropped.is_empty() {
            fixes.push(PlanFix::TooMany { dropped });
        }
    }

    fn longer_first(&mut self, fixes: &mut Vec<PlanFix>) {
        let mut raised = Vec::new();
        let mut longest: Option<CacheTtl> = None;
        for (at, ttl) in self.in_order().into_iter().rev() {
            match longest {
                Some(l) if l.lifetime() > ttl.lifetime() => {
                    *self.ttl_mut(at).expect("a breakpoint in the plan") = l;
                    raised.push(at);
                }
                _ => longest = Some(ttl),
            }
        }
        if !raised.is_empty() {
            raised.reverse();
            fixes.push(PlanFix::Raised { at: raised });
        }
    }
}
