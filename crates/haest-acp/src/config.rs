//! A session's modes (haest's plan mode) and its config options (the model
//! and the reasoning effort), as the client shows and sets them.

use agent_client_protocol::v1::schema::{
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectGroup,
    SessionConfigSelectOption, SessionMode, SessionModeState,
};
use haest_core::Effort;
use haest_providers::Catalog;

/// The mode every tool runs in.
pub(crate) const DEFAULT_MODE: &str = "default";
/// haest's plan mode: read-only until it is turned off.
pub(crate) const PLAN_MODE: &str = "plan";

/// The model option's id.
pub(crate) const MODEL: &str = "model";
/// The effort option's id.
pub(crate) const EFFORT: &str = "effort";
/// The effort option's value for the model's own default.
const MODEL_DEFAULT: &str = "default";

/// The two modes, `plan_on` saying which is current.
pub(crate) fn modes(plan_on: bool) -> SessionModeState {
    let current = if plan_on { PLAN_MODE } else { DEFAULT_MODE };
    SessionModeState::new(
        current,
        vec![
            SessionMode::new(DEFAULT_MODE, "Default").description("Every tool runs."),
            SessionMode::new(PLAN_MODE, "Plan")
                .description("Read-only: haest plans and changes nothing until plan mode is off."),
        ],
    )
}

/// The mode id's `/plan` argument: on for plan, off for default; None for
/// a mode haest has not.
pub(crate) fn plan_argument(mode: &str) -> Option<&'static str> {
    match mode {
        PLAN_MODE => Some(""),
        DEFAULT_MODE => Some("off"),
        _ => None,
    }
}

/// The model picker over the built-in catalog, grouped as `haest models`
/// lists it, with the current model first when the catalog has not it (a
/// providers.json model, a spec the catalog does not know); and the effort.
pub(crate) fn options(model_spec: &str, effort: Option<Effort>) -> Vec<SessionConfigOption> {
    let catalog = Catalog::builtin();
    let mut groups: Vec<SessionConfigSelectGroup> = Vec::new();
    if !catalog.models().any(|m| m.spec == model_spec) {
        let current =
            SessionConfigSelectOption::new(model_spec.to_string(), model_spec.to_string());
        groups.push(SessionConfigSelectGroup::new(
            "current",
            "Current",
            vec![current],
        ));
    }
    for (n, group) in catalog.groups().iter().enumerate() {
        let models = group
            .models
            .iter()
            .map(|m| SessionConfigSelectOption::new(m.spec.clone(), m.label.clone()))
            .collect();
        groups.push(SessionConfigSelectGroup::new(
            format!("group-{n}"),
            group.label.clone(),
            models,
        ));
    }
    let model = SessionConfigOption::select(MODEL, "Model", model_spec.to_string(), groups)
        .category(SessionConfigOptionCategory::Model);

    let mut efforts = vec![SessionConfigSelectOption::new(
        MODEL_DEFAULT,
        "Model default",
    )];
    efforts.extend(
        Effort::ALL
            .iter()
            .map(|e| SessionConfigSelectOption::new(e.as_str(), e.as_str())),
    );
    let current = effort.map_or(MODEL_DEFAULT, Effort::as_str);
    let effort = SessionConfigOption::select(EFFORT, "Effort", current, efforts)
        .description("How much the model reasons before it answers.")
        .category(SessionConfigOptionCategory::ThoughtLevel);
    vec![model, effort]
}

/// The effort an effort option's value sets: `Ok(None)` for the model's own
/// default, `Err` for a value that is no effort.
pub(crate) fn parse_effort(value: &str) -> Result<Option<Effort>, ()> {
    if value == MODEL_DEFAULT {
        return Ok(None);
    }
    value.parse::<Effort>().map(Some).map_err(drop)
}
