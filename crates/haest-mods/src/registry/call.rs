//! The tool events across every mod, for [`ModTools`](crate::ModTools):
//! `tool.before` (gates last; the first deny wins, input rewrites chain) and
//! `tool.after` (output rewrites chain).

use std::ops::ControlFlow;

use haest_core::{
    error::report,
    tool::{ToolContext, ToolError, ToolOutput},
};

use super::{Mods, nested::Reported};
use crate::{api::ModToolCall, events::ModEvent};

/// What the model reads for a call the human interrupted while a mod was deciding.
pub(super) const INTERRUPTED: &str = "Interrupted by the user before this tool ran.";

/// What `tool.before` across every mod decided about one call.
pub(super) enum Judged {
    /// Run it, with the input the mods left.
    Run(ModToolCall),
    /// A mod refused it, for this reason.
    Refused(String),
    /// The human interrupted the call while a mod was deciding.
    Interrupted,
    /// A mod failed, which is reported: the call does not run.
    Failed(Reported),
}

impl Mods {
    /// `tool.before` across every mod: the call to run, or why it must not run.
    /// A mod that fails refuses the call -- a guard that broke must not wave
    /// anything through.
    pub(crate) async fn before(
        &self,
        call: ModToolCall,
        ctx: &ToolContext,
    ) -> Result<ModToolCall, ToolError> {
        match self.judge(call, ctx).await {
            Judged::Run(call) => Ok(call),
            Judged::Refused(reason) => Err(ToolError::Input(format!("Refused: {reason}"))),
            Judged::Interrupted => Err(ToolError::Input(INTERRUPTED.into())),
            Judged::Failed(failed) => Err(ToolError::Failed(format!(
                "the mod {} failed before this call, so it did not run: {}",
                failed.name,
                report(&*failed.error)
            ))),
        }
    }

    /// What every mod's `tool.before` decided about `call`, gates last; a
    /// failure is reported here, once.
    pub(super) async fn judge(&self, mut call: ModToolCall, ctx: &ToolContext) -> Judged {
        let (gates, rest): (Vec<_>, Vec<_>) = self.list().iter().partition(|m| m.is_gate());
        for m in rest.into_iter().chain(gates) {
            match m.tool_before(&call, ctx, self.env()).await {
                Ok(decision) => {
                    if let ControlFlow::Break(reason) = decision.apply_to(&mut call) {
                        return Judged::Refused(reason);
                    }
                }
                Err(_) if ctx.cancel().is_cancelled() => return Judged::Interrupted,
                Err(e) => {
                    self.report(m.as_ref(), ModEvent::ToolBefore.as_str(), &e);
                    return Judged::Failed(match e.downcast::<Reported>() {
                        Ok(nested) => *nested,
                        Err(error) => Reported {
                            name: m.name().to_string(),
                            error,
                        },
                    });
                }
            }
        }
        Judged::Run(call)
    }

    /// `tool.after` across every mod. The tool DID run: a mod that fails leaves
    /// the output as it was, so the model never retries a call that happened.
    /// A rewrite changes what the model reads, never what the call spent: the
    /// tool's `cost_usd` and `child_cost_usd` stay, whatever the mod's output says.
    pub(crate) async fn after(
        &self,
        call: &ModToolCall,
        mut output: ToolOutput,
        ctx: &ToolContext,
    ) -> ToolOutput {
        for m in self.list() {
            match m.tool_after(call, &output, ctx, self.env()).await {
                Ok(Some(replaced)) => {
                    output = ToolOutput {
                        cost_usd: output.cost_usd,
                        child_cost_usd: output.child_cost_usd,
                        ..replaced
                    };
                }
                Ok(None) => {}
                Err(_) if ctx.cancel().is_cancelled() => {}
                Err(e) => self.report(m.as_ref(), ModEvent::ToolAfter.as_str(), &e),
            }
        }
        output
    }
}
