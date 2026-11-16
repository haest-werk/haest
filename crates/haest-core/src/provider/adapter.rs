//! What every provider adapter does at the end of a failed stream. Reached
//! through [`__private`](crate::__private).

use std::fmt;

use tokio_util::sync::CancellationToken;

use crate::messages::{AssistantMessage, StopReason};

/// Marks `msg` as a call that failed, for the `Done` an adapter sends at the
/// end: [`StopReason::Aborted`] when `cancel` has fired (the error is then the
/// interrupt's echo and is dropped), else [`StopReason::Error`] with `err` as
/// its `error_message`.
///
/// # Examples
///
/// ```
/// use haest_core::{AssistantMessage, StopReason, __private::mark_stream_failed};
/// use tokio_util::sync::CancellationToken;
///
/// let cancel = CancellationToken::new();
/// let mut msg = AssistantMessage::start("example", "model-1");
/// mark_stream_failed(&mut msg, &cancel, "HTTP 529: overloaded");
/// assert_eq!(msg.stop_reason, StopReason::Error);
/// assert_eq!(msg.error_message.as_deref(), Some("HTTP 529: overloaded"));
///
/// cancel.cancel();
/// let mut msg = AssistantMessage::start("example", "model-1");
/// mark_stream_failed(&mut msg, &cancel, "request aborted");
/// assert_eq!((msg.stop_reason, msg.error_message), (StopReason::Aborted, None));
/// ```
pub fn mark_stream_failed(
    msg: &mut AssistantMessage,
    cancel: &CancellationToken,
    err: impl fmt::Display,
) {
    if cancel.is_cancelled() {
        msg.stop_reason = StopReason::Aborted;
    } else {
        msg.stop_reason = StopReason::Error;
        msg.error_message = Some(err.to_string());
    }
}
