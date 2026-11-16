//! Which tool calls of one model response may run at the same time.
//!
//! A model that asks for five Reads in one response waits for five round trips
//! when they run one by one. Calls run together only when that cannot change
//! the answer: every call in the batch is `read_only` (nothing it does is seen by
//! another), and the batch never reorders around a write -- a write is a batch of
//! one, so "Read, Edit, Read" still reads the edited file the second time.
//!
//! An unknown tool name is a batch of one too -- it answers with an error anyway.
//!
//! The loop runs every model response's calls through [`run_tool_batches`],
//! which delivers each [`Answer`] to an [`Answers`] in call order.
//! [`tool_batches`] is the split alone.

use std::time::Instant;

use futures::{StreamExt, stream::FuturesOrdered};

use crate::{
    messages::{ToolCall, Usage},
    tool::{ToolContext, ToolOutput},
    tool_exec::{ToolIndex, fold_spend, run_call},
};

/// `calls` split into consecutive batches, in call order.
///
/// A run of calls to read-only tools ([`Tool::read_only`](crate::Tool::read_only))
/// is one batch; every other call, and every call to a name `tools` does
/// not know, is a batch of its own.
pub fn tool_batches<'c>(calls: &'c [ToolCall], tools: &ToolIndex) -> Vec<Vec<&'c ToolCall>> {
    let mut out: Vec<Vec<&ToolCall>> = Vec::new();
    let mut open = false;
    for call in calls {
        if !tools.get(&call.name).is_some_and(|t| t.read_only()) {
            open = false;
            out.push(vec![call]);
            continue;
        }
        match out.last_mut() {
            Some(batch) if open => batch.push(call),
            _ => {
                open = true;
                out.push(vec![call]);
            }
        }
    }
    out
}

/// The context room each call of a batch may fill: shared, so N parallel Reads
/// together stay under the cap.
fn room_share(room: Option<u64>, batch_size: usize) -> Option<u64> {
    room.map(|r| r / batch_size.max(1) as u64)
}

/// One finished tool call, as [`run_tool_batches`] delivers it.
#[derive(Debug, Clone)]
pub struct Answer {
    /// The call.
    pub call: ToolCall,
    /// What it produced, already spilled to a file if it was too big.
    pub output: ToolOutput,
    /// From the start of the call's batch to its answer.
    pub duration_ms: u64,
}

/// Where the answers of [`run_tool_batches`] go.
pub trait Answers: Send {
    /// Tokens the next batch may add to the context; read once per batch, after
    /// every answer before it was delivered. None = uncapped.
    fn room(&self) -> Option<u64>;
    /// One answer, in CALL order.
    fn answer(&mut self, answer: Answer);
}

/// Run `calls` batch by batch and deliver every answer in CALL order -- history,
/// the guard's repeat count and the transcript all read them that way -- each one
/// as soon as every call before it has answered. A tool's own spend is folded
/// into `usage` as its answer is delivered.
///
/// Each call gets a clone of `ctx` with its own call id and its share of
/// [`Answers::room`]. Every call is answered, failures and panics included; a
/// call that starts after `ctx.cancel` fired is answered as interrupted
/// without running.
pub async fn run_tool_batches(
    calls: &[ToolCall],
    tools: &ToolIndex,
    ctx: &ToolContext,
    usage: &mut Usage,
    to: &mut dyn Answers,
) {
    for batch in tool_batches(calls, tools) {
        let room = room_share(to.room(), batch.len());
        let started = Instant::now();
        let mut running: FuturesOrdered<_> = batch
            .into_iter()
            .map(|call| {
                let mut ctx = ctx.clone();
                ctx.context_room_tokens = room;
                ctx.tool_call_id = Some(call.id.clone());
                async move {
                    let output = run_call(tools, call, &ctx).await;
                    (call, output, started.elapsed().as_millis() as u64)
                }
            })
            .collect();
        while let Some((call, output, duration_ms)) = running.next().await {
            fold_spend(usage, &output);
            to.answer(Answer {
                call: call.clone(),
                output,
                duration_ms,
            });
        }
    }
}

#[cfg(test)]
mod tests;
