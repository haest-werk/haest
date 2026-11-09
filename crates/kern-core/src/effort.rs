//! Fitting the effort to what the current model accepts. Pure.
//!
//! Like media fitting, this happens per REQUEST and never touches the stored
//! setting: `off` stays `off`, so switching to a model that can stop thinking
//! gets `off` again.
//!
//! The one fit today: `off` on a model whose reasoning is mandatory. OpenRouter
//! answers `enabled: false` there with a 400 on every turn, so the model gets
//! the cheapest thinking it takes instead.

use crate::{
    catalog::{OpenRouterEffort, Reasoning},
    deps::AgentHooks,
    provider::Effort,
};

/// kern's own levels, cheapest first. OpenRouter's `minimal` has no kern name, so the floor is `low`.
const LEVELS: [Effort; 5] = [
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::Xhigh,
    Effort::Max,
];

/// The OpenRouter name of a kern level (`Off` has none: it is `None` only where listed).
pub fn as_openrouter(e: Effort) -> Option<OpenRouterEffort> {
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

fn cheapest_level(r: &Reasoning) -> Effort {
    LEVELS
        .into_iter()
        .find(|l| listed(r, *l))
        .unwrap_or(Effort::Low)
}

/// The effort THIS model may receive for `spec`; `effort` as asked whenever the catalog has no objection.
pub fn fit_effort(
    effort: Option<Effort>,
    spec: &str,
    model: &str,
    hooks: &dyn AgentHooks,
) -> Option<Effort> {
    if effort != Some(Effort::Off) {
        return effort;
    }
    let Some(reasoning) = hooks.reasoning(spec).filter(|r| r.mandatory) else {
        return effort;
    };
    let used = cheapest_level(&reasoning);
    hooks.on_effort_fitted(model, used);
    Some(used)
}

/// For a wire dialect that 400s on any level it does not list (a direct
/// `reasoning_effort` API, unlike OpenRouter which maps them): the accepted
/// level nearest `effort`, a tie going UP. `Off` is `none` where listed, the
/// cheapest level when reasoning is mandatory, else None = send no level at all.
/// None too when nothing kern knows is listed.
pub fn nearest_accepted_effort(effort: Effort, accepted: &Reasoning) -> Option<OpenRouterEffort> {
    if effort == Effort::Off {
        if accepted.efforts.contains(&OpenRouterEffort::None) {
            return Some(OpenRouterEffort::None);
        }
        return if accepted.mandatory {
            as_openrouter(cheapest_level(accepted))
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
    use crate::tool::ToolHost;

    fn r(mandatory: bool, efforts: &[OpenRouterEffort]) -> Reasoning {
        Reasoning {
            mandatory,
            efforts: efforts.to_vec(),
        }
    }

    struct Mandatory(Mutex<Vec<Effort>>);
    impl ToolHost for Mandatory {}
    impl AgentHooks for Mandatory {
        fn reasoning(&self, _: &str) -> Option<Reasoning> {
            Some(r(true, &[OpenRouterEffort::Medium, OpenRouterEffort::High]))
        }
        fn on_effort_fitted(&self, _: &str, used: Effort) {
            self.0.lock().unwrap().push(used);
        }
    }

    #[test]
    fn off_on_a_mandatory_model_becomes_its_cheapest_level_and_says_so() {
        let hooks = Mandatory(Mutex::default());
        assert_eq!(
            fit_effort(Some(Effort::Off), "s", "m", &hooks),
            Some(Effort::Medium)
        );
        assert_eq!(*hooks.0.lock().unwrap(), [Effort::Medium]);
        assert_eq!(
            fit_effort(Some(Effort::High), "s", "m", &hooks),
            Some(Effort::High)
        );
        assert_eq!(fit_effort(None, "s", "m", &hooks), None);
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
}
