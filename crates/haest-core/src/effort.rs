//! Fitting the reasoning level ([`Effort`]) to what the current model accepts,
//! by its catalog row ([`Reasoning`]). Pure.
//!
//! Like media fitting ([`crate::media`]), this happens per REQUEST and never
//! touches the stored setting: `off` stays `off`, so switching to a model that
//! can stop thinking gets `off` again.
//!
//! The loop's one fit, [`fit_effort`]: `off` on a model whose reasoning is
//! mandatory. OpenRouter answers `enabled: false` there with a 400 on every
//! turn, so the model gets the cheapest thinking it takes instead.
//! [`nearest_accepted_effort`] is for provider adapters (`haest-providers`)
//! whose API refuses any level it does not list.

use crate::{
    catalog::{ModelFacts, OpenRouterEffort, Reasoning},
    deps::AgentHooks,
    provider::Effort,
};

/// haest's own levels, cheapest first. OpenRouter's `minimal` has no haest name, so the floor is `low`.
const LEVELS: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// The OpenRouter level with the same name as haest's `e`; None for
/// [`Effort::Off`], which maps to [`OpenRouterEffort::None`] only where a
/// model lists it ([`nearest_accepted_effort`] decides that).
pub(crate) fn as_openrouter(e: Effort) -> Option<OpenRouterEffort> {
    Some(match e {
        Effort::Off => return None,
        Effort::Low => OpenRouterEffort::Low,
        Effort::Medium => OpenRouterEffort::Medium,
        Effort::High => OpenRouterEffort::High,
        Effort::Xhigh => OpenRouterEffort::Xhigh,
        Effort::Max => OpenRouterEffort::Max,
    })
}

fn listed(r: &Reasoning, e: Effort) -> bool {
    as_openrouter(e).is_some_and(|o| r.efforts.contains(&o))
}

/// The cheapest of haest's levels `r` lists, if it lists any.
fn cheapest_listed(r: &Reasoning) -> Option<Effort> {
    LEVELS.into_iter().find(|l| listed(r, *l))
}

/// The effort the model behind `spec` may receive: `effort` as asked unless it
/// is [`Effort::Off`] and the host's catalog row says reasoning is mandatory.
///
/// Then the answer is the cheapest haest level the row lists (`low` when it
/// lists none), and [`AgentHooks::on_effort_fitted`] hears about it with
/// `model` (the provider's model id, for the human). The row comes from
/// [`ModelFacts::reasoning`]; a host that returns None there gets `effort`
/// back unchanged. None stays None: the model's own default.
pub(crate) fn fit_effort(
    effort: Option<Effort>,
    spec: &str,
    model: &str,
    facts: &dyn ModelFacts,
    hooks: &dyn AgentHooks,
) -> Option<Effort> {
    if effort != Some(Effort::Off) {
        return effort;
    }
    let Some(reasoning) = facts.reasoning(spec).filter(|r| r.mandatory) else {
        return effort;
    };
    let used = cheapest_listed(&reasoning).unwrap_or(Effort::Low);
    hooks.on_effort_fitted(model, used);
    Some(used)
}

/// The level `accepted` lists that is nearest `effort`, a tie going UP; None
/// means send no level at all.
///
/// For a wire dialect that 400s on any level it does not list (a direct
/// `reasoning_effort` API, unlike OpenRouter, which maps them). Only haest's
/// own levels (`low` to `max`) are candidates, never `minimal`, so a row that
/// lists none of them gives None.
///
/// [`Effort::Off`] is `none` where listed; else, when reasoning is mandatory,
/// the cheapest of haest's levels the row lists; else None.
///
/// # Examples
///
/// ```
/// use haest_core::{
///     Effort, Reasoning,
///     catalog::OpenRouterEffort as O,
///     provider::nearest_accepted_effort,
/// };
///
/// let row = Reasoning::new([O::Low, O::High]);
/// // `medium` is one step from both: the tie goes up.
/// assert_eq!(nearest_accepted_effort(Effort::Medium, &row), Some(O::High));
/// assert_eq!(nearest_accepted_effort(Effort::Max, &row), Some(O::High));
/// // Off, on a model that may stop thinking but lists no `none`: send nothing.
/// assert_eq!(nearest_accepted_effort(Effort::Off, &row), None);
/// ```
pub fn nearest_accepted_effort(effort: Effort, accepted: &Reasoning) -> Option<OpenRouterEffort> {
    if effort == Effort::Off {
        if accepted.efforts.contains(&OpenRouterEffort::None) {
            return Some(OpenRouterEffort::None);
        }
        return if accepted.mandatory {
            cheapest_listed(accepted).and_then(as_openrouter)
        } else {
            None
        };
    }
    if listed(accepted, effort) {
        return as_openrouter(effort);
    }
    let at = LEVELS.iter().position(|l| *l == effort).unwrap_or(0) as i32;
    let mut best: Option<(usize, Effort)> = None;
    for (i, level) in LEVELS.into_iter().enumerate() {
        if !listed(accepted, level) {
            continue;
        }
        let closer = best.is_none_or(|(b, _)| (i as i32 - at).abs() <= (b as i32 - at).abs());
        if closer {
            best = Some((i, level));
        }
    }
    best.and_then(|(_, l)| as_openrouter(l))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn r(mandatory: bool, efforts: &[OpenRouterEffort]) -> Reasoning {
        Reasoning::new(efforts.iter().copied()).with_mandatory(mandatory)
    }

    struct Mandatory(Mutex<Vec<Effort>>);
    impl ModelFacts for Mandatory {
        fn reasoning(&self, _: &str) -> Option<Reasoning> {
            Some(r(true, &[OpenRouterEffort::Medium, OpenRouterEffort::High]))
        }
    }
    impl AgentHooks for Mandatory {
        fn on_effort_fitted(&self, _: &str, used: Effort) {
            self.0.lock().unwrap().push(used);
        }
    }

    #[test]
    fn off_on_a_mandatory_model_becomes_its_cheapest_level_and_says_so() {
        let hooks = Mandatory(Mutex::default());
        assert_eq!(
            fit_effort(Some(Effort::Off), "s", "m", &hooks, &hooks),
            Some(Effort::Medium)
        );
        assert_eq!(*hooks.0.lock().unwrap(), [Effort::Medium]);
        assert_eq!(
            fit_effort(Some(Effort::High), "s", "m", &hooks, &hooks),
            Some(Effort::High)
        );
        assert_eq!(fit_effort(None, "s", "m", &hooks, &hooks), None);
    }

    #[test]
    fn the_nearest_listed_level_wins_and_a_tie_goes_up() {
        use OpenRouterEffort as O;
        let low_high = r(false, &[O::Low, O::High]);
        assert_eq!(
            nearest_accepted_effort(Effort::Medium, &low_high),
            Some(O::High)
        );
        assert_eq!(
            nearest_accepted_effort(Effort::Max, &low_high),
            Some(O::High)
        );
        assert_eq!(nearest_accepted_effort(Effort::Off, &low_high), None);
        assert_eq!(
            nearest_accepted_effort(Effort::Off, &r(true, &[O::High])),
            Some(O::High)
        );
        assert_eq!(
            nearest_accepted_effort(Effort::Off, &r(false, &[O::None])),
            Some(O::None)
        );
        assert_eq!(
            nearest_accepted_effort(Effort::Low, &r(false, &[O::Minimal])),
            None
        );
    }

    #[test]
    fn off_on_a_mandatory_row_with_none_of_haests_levels_sends_no_level() {
        use OpenRouterEffort as O;
        assert_eq!(
            nearest_accepted_effort(Effort::Off, &r(true, &[O::Minimal])),
            None
        );
        assert_eq!(nearest_accepted_effort(Effort::Off, &r(true, &[])), None);
    }
}
