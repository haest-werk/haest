//! A session's mode and config options set by the client, and the saved
//! sessions listed for it.

use std::path::PathBuf;

use agent_client_protocol::{
    Error, ErrorCode,
    v1::schema::{
        ListSessionsResponse, SessionConfigId, SessionConfigOptionValue, SessionInfo,
        SessionModeId, SetSessionConfigOptionResponse, SetSessionModeResponse,
    },
};
use haest_core::{
    error::report,
    session::{iso_utc, sessions_dir},
};
use haest_mods::PLAN_COMMAND;
use haest_runtime::list_sessions;
use haest_session::Control;

use crate::{
    Setup, config,
    prompt::session_failed,
    state::{Live, State},
};

/// Switch plan mode as `mode` says: the `/plan` command, so plan mode's
/// listener tells the client the mode, as when the human types it.
pub(crate) async fn set_mode(
    live: &Live,
    mode: &SessionModeId,
) -> Result<SetSessionModeResponse, Error> {
    let Some(args) = config::plan_argument(&mode.0) else {
        return Err(Error::invalid_params().data(format!("haest has no mode {}", mode.0)));
    };
    live.session
        .with(move |rt| Box::pin(async move { rt.mods().command(PLAN_COMMAND, args).await }))
        .await
        .map_err(session_failed)?;
    Ok(SetSessionModeResponse::new())
}

/// Set the model or the effort from the next turn on, and answer with every
/// option as it is now.
pub(crate) async fn set_option(
    live: &Live,
    id: &SessionConfigId,
    value: &SessionConfigOptionValue,
) -> Result<SetSessionConfigOptionResponse, Error> {
    let Some(value) = value.as_value_id() else {
        return Err(Error::invalid_params().data("haest's options are picked from a list"));
    };
    let value = value.0.to_string();
    let control = match &*id.0 {
        config::MODEL => Control::SetModelSpec(value),
        config::EFFORT => match config::parse_effort(&value) {
            Ok(effort) => Control::SetEffort(effort),
            Err(()) => {
                return Err(Error::invalid_params().data(format!("{value} is no effort")));
            }
        },
        other => return Err(Error::invalid_params().data(format!("haest has no option {other}"))),
    };
    // A model it cannot use (no key, an unknown provider) is refused, and
    // the conversation stays on its model.
    live.session
        .control(control)
        .await
        .map_err(|e| Error::new(ErrorCode::INVALID_PARAMS, report(&e)))?;
    let now = live.session.snapshot().await.map_err(session_failed)?;
    let effort = now.effort.as_deref().and_then(|e| e.parse().ok());
    Ok(SetSessionConfigOptionResponse::new(config::options(
        &now.model_spec,
        effort,
    )))
}

/// The saved sessions, newest first: those that started in `cwd` when the
/// client names one, else all of them. The sessions dir is the settings' of
/// the client's directory (the agent's own when it names none).
pub(crate) async fn list(
    state: &State,
    cwd: Option<PathBuf>,
) -> Result<ListSessionsResponse, Error> {
    let setup = Setup::new(cwd.clone().unwrap_or_else(|| state.cwd.clone()));
    let inputs = state
        .conversations
        .inputs(&setup)
        .await
        .map_err(|e| Error::new(ErrorCode::INTERNAL_ERROR, report(&*e)))?;
    let dir = sessions_dir(&inputs.settings().data_dir);
    let sessions = list_sessions(inputs.world().fs().as_ref(), &dir)
        .await
        .into_iter()
        .filter(|s| cwd.as_ref().is_none_or(|cwd| &s.cwd == cwd))
        .map(|s| {
            let info = SessionInfo::new(s.id, s.cwd).updated_at(iso_utc(s.modified_ms));
            match s.preview.trim() {
                "" => info,
                preview => info.title(preview.to_string()),
            }
        })
        .collect();
    Ok(ListSessionsResponse::new(sessions))
}
