//! [`OnResume`]: what a resumed durable run does with a call of a tool.

#[cfg(doc)]
use super::{Tool, ToolContext};

/// What a resumed durable run ([`crate::run`]) does with a call that may have
/// run before the stop ([`Tool::on_resume`]).
///
/// # Examples
///
/// A tool that sends a payment hands the API its effect key, so running it
/// again is safe; on resume it asks instead:
///
/// ```
/// use async_trait::async_trait;
/// use haest_core::{Tool, ToolContext, ToolError, ToolOutput, tool::{JsonMap, OnResume}};
/// use serde_json::{Value, json};
///
/// struct Pay;
///
/// #[async_trait]
/// impl Tool for Pay {
///     fn name(&self) -> &str { "Pay" }
///     fn description(&self) -> String { "Send a payment.".into() }
///     fn parameters(&self) -> Value { json!({"type": "object"}) }
///     fn kind(&self) -> &str { "payment.send" }
///     fn on_resume(&self) -> OnResume {
///         OnResume::Lookup
///     }
///     async fn execute(&self, _input: JsonMap, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
///         let key = ctx.effect_key().map(|k| k.to_string()).unwrap_or_default();
///         if ctx.resumed {
///             // Ask the payment API what happened under `key`.
///             return Ok(ToolOutput::text(format!("looked up {key}")));
///         }
///         // Send it with `key` as the API's idempotency key.
///         Ok(ToolOutput::text(format!("sent {key}")))
///     }
/// }
///
/// assert_eq!(Pay.on_resume(), OnResume::Lookup);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnResume {
    /// Running it again is safe (a read, a Write of the same bytes): it runs
    /// again, with [`ToolContext::resumed`] set.
    Rerun,
    /// It is answered as interrupted: it may or may not have run. What every
    /// call without a result got before durable runs.
    Report,
    /// The tool can find out what happened: it runs again with
    /// [`ToolContext::resumed`] set and the same
    /// [`effect key`](ToolContext::effect_key), and asks its backend.
    Lookup,
}
