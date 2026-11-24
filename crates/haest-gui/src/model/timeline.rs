//! One agent's turns as tracks of bars: model calls on the first lane, tool
//! calls below it, the calls of one batch stacked one lane each.
//!
//! Times are the frames' own `ts` (ms since the epoch). A model call runs
//! from its `model_start` to its `assistant`; a tool call from its batch's
//! start (`tool_result.ts - durationMs`) to its answer. Until a call ends it
//! runs to "now", which the view supplies.

use haest::core::{LoopEvent, messages::Usage};

/// A model call.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCall {
    /// Its `model_start`.
    pub start_ts: u64,
    /// Its `assistant`, once it came.
    pub end_ts: Option<u64>,
    /// The model it called, as `provider:model`.
    pub model: String,
    /// The call's own usage, from its `assistant` frame.
    pub usage: Option<Usage>,
    /// It failed or never went out.
    pub error: bool,
}

/// A tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolBar {
    /// The call's id.
    pub call_id: String,
    /// The tool's name.
    pub name: String,
    /// The batch it ran in, within its turn.
    pub batch: usize,
    /// Its place in that batch: the lane under the model calls.
    pub lane: usize,
    /// Its batch's start.
    pub start_ts: u64,
    /// Its answer, once it came.
    pub end_ts: Option<u64>,
    /// It failed.
    pub error: bool,
}

/// One turn: a track.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    /// 1, 2, ... in this stream.
    pub number: usize,
    /// When it started.
    pub start_ts: u64,
    /// When it ended, once it did.
    pub end_ts: Option<u64>,
    /// Its model calls, in order.
    pub calls: Vec<ModelCall>,
    /// Its tool calls, in order.
    pub tools: Vec<ToolBar>,
    /// The whole turn's usage, from `turn_end`.
    pub usage: Option<Usage>,
    /// Why it stopped, once it did.
    pub stop_reason: Option<String>,
    /// Frames of it were lost: its bars are incomplete.
    pub partial: bool,
    batches: usize,
}

impl Turn {
    /// How many lanes it needs: the model lane and its widest batch.
    pub fn lanes(&self) -> usize {
        1 + self.tools.iter().map(|t| t.lane + 1).max().unwrap_or(0)
    }

    /// When it ended, or `now` while it runs.
    pub fn end_or(&self, now: u64) -> u64 {
        self.end_ts.unwrap_or(now).max(self.start_ts)
    }
}

/// Of a call's input, the shares the prompt cache served, wrote, and the
/// share sent uncached; None when the call reported no input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CacheSplit {
    /// Read from the prompt cache: the cheap part.
    pub served: f32,
    /// Written to it: a one-time premium.
    pub written: f32,
    /// Sent uncached.
    pub uncached: f32,
}

impl CacheSplit {
    /// The split of `usage`'s input.
    pub fn of(usage: &Usage) -> Option<Self> {
        let total = usage.input + usage.cache_read + usage.cache_write;
        if total == 0 {
            return None;
        }
        let share = |n: u64| n as f32 / total as f32;
        Some(Self {
            served: share(usage.cache_read),
            written: share(usage.cache_write),
            uncached: share(usage.input),
        })
    }
}

/// One agent's turns.
#[derive(Debug, Default, Clone)]
pub struct Timeline {
    /// Its turns, oldest first.
    pub turns: Vec<Turn>,
    open: bool,
}

impl Timeline {
    /// No turns yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The turn running now, opened at `ts` when none is.
    fn running(&mut self, ts: u64) -> &mut Turn {
        if !self.open {
            self.open = true;
            let number = self.turns.len() + 1;
            self.turns.push(Turn {
                number,
                start_ts: ts,
                end_ts: None,
                calls: Vec::new(),
                tools: Vec::new(),
                usage: None,
                stop_reason: None,
                partial: false,
                batches: 0,
            });
        }
        self.turns.last_mut().expect("a turn was just opened")
    }

    /// Frames were lost: the turn that runs is missing some.
    pub fn lost(&mut self) {
        if self.open
            && let Some(turn) = self.turns.last_mut()
        {
            turn.partial = true;
        }
    }

    /// The event `ev`, stamped `ts`.
    pub fn event(&mut self, ev: &LoopEvent, ts: u64) {
        match ev {
            // The turn's input opens it; steering mid-turn does not.
            LoopEvent::User { .. } => {
                self.running(ts);
            }
            LoopEvent::ModelStart { provider, model } => {
                let turn = self.running(ts);
                turn.calls.push(ModelCall {
                    start_ts: ts,
                    end_ts: None,
                    model: format!("{provider}:{model}"),
                    usage: None,
                    error: false,
                });
            }
            LoopEvent::Assistant { message } => {
                let turn = self.running(ts);
                let error = message.stop_reason == haest::core::messages::StopReason::Error;
                match turn.calls.iter_mut().rev().find(|c| c.end_ts.is_none()) {
                    Some(call) => {
                        call.end_ts = Some(ts);
                        call.usage = Some(message.usage.clone());
                        call.error = error;
                    }
                    // A call that never went out: no model_start before it.
                    None => turn.calls.push(ModelCall {
                        start_ts: ts,
                        end_ts: Some(ts),
                        model: format!("{}:{}", message.provider, message.model),
                        usage: Some(message.usage.clone()),
                        error,
                    }),
                }
                let batch = turn.batches;
                let mut lane = 0;
                for call in message.tool_calls() {
                    turn.tools.push(ToolBar {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        batch,
                        lane,
                        start_ts: ts,
                        end_ts: None,
                        error: false,
                    });
                    lane += 1;
                }
                if lane > 0 {
                    turn.batches += 1;
                }
            }
            LoopEvent::ToolResult {
                call,
                output,
                message,
                duration_ms,
            } => {
                let turn = self.running(ts);
                let start = ts.saturating_sub(*duration_ms).max(turn.start_ts);
                let error = output.is_error || message.is_error;
                match turn.tools.iter_mut().rev().find(|t| t.call_id == call.id) {
                    Some(bar) => {
                        bar.start_ts = start;
                        bar.end_ts = Some(ts);
                        bar.error = error;
                    }
                    None => {
                        let batch = turn.batches;
                        turn.batches += 1;
                        turn.tools.push(ToolBar {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                            batch,
                            lane: 0,
                            start_ts: start,
                            end_ts: Some(ts),
                            error,
                        });
                    }
                }
            }
            LoopEvent::TurnEnd {
                stop_reason, usage, ..
            } => {
                let turn = self.running(ts);
                turn.end_ts = Some(ts);
                turn.usage = Some(usage.clone());
                turn.stop_reason = Some(stop_reason.as_str().to_string());
                // Whatever still ran ends with it (an aborted turn).
                for call in &mut turn.calls {
                    call.end_ts.get_or_insert(ts);
                }
                for bar in &mut turn.tools {
                    bar.end_ts.get_or_insert(ts);
                }
                self.open = false;
            }
            _ => {}
        }
    }

    /// The longest turn, ms (a running one up to `now`): what "fit" shows.
    pub fn longest_ms(&self, now: u64) -> u64 {
        self.turns
            .iter()
            .map(|t| t.end_or(now) - t.start_ts)
            .max()
            .unwrap_or(0)
    }

    /// Whether a turn runs now.
    pub fn is_running(&self) -> bool {
        self.open
    }
}
