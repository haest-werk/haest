//! A mod file's handlers on one event, as haest-mods' folds ask them: one
//! call into its runtime per handler (bootstrap.js runs that one handler
//! and hands back its answer as it was returned), the answer read as the
//! event's decision by haest-mods' one decoder. Nothing on this side or in
//! bootstrap.js reads an answer another way.

use std::{marker::PhantomData, sync::Mutex};

use async_trait::async_trait;
use haest_core::error::BoxError;
use haest_mods::{__private::fold::Handlers, Event};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    channel::{JsMeta, JsThread},
    returns::BadReturn,
};

/// A mod file's handlers on `E`, as many as it has now. A handler that
/// throws or answers what `E` does not take fails the mod's event: the
/// handlers after it are not asked.
pub(super) struct JsHandlers<'a, E> {
    thread: &'a JsThread,
    count: usize,
    cancel: Option<&'a CancellationToken>,
    event: PhantomData<fn() -> E>,
}

impl<'a, E: Event> JsHandlers<'a, E> {
    /// The handlers on `E` the mod behind `thread` registered in `meta`,
    /// asked under `cancel` (a wait it ends).
    pub(super) fn new(
        thread: &'a JsThread,
        meta: &Mutex<JsMeta>,
        cancel: Option<&'a CancellationToken>,
    ) -> Self {
        Self {
            thread,
            count: JsMeta::lock(meta).handlers(E::NAME),
            cancel,
            event: PhantomData,
        }
    }
}

#[async_trait]
impl<E: Event> Handlers<E> for JsHandlers<'_, E> {
    type Error = BoxError;

    fn count(&self) -> usize {
        self.count
    }

    async fn ask(&self, nth: usize, payload: E) -> Result<Option<E::Decision>, BoxError> {
        let event = E::NAME;
        let payload =
            serde_json::to_value(payload).map_err(|source| BadReturn::Payload { event, source })?;
        let asked = json!({ "nth": nth, "payload": payload });
        let got = self.thread.call(event.as_str(), asked, self.cancel).await?;
        let decision =
            E::read_decision(got).map_err(|source| BadReturn::Answer { event, source })?;
        Ok(Some(decision))
    }
}
