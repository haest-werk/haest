//! [`Hooks`](super::Hooks): what a failed or late hook does under each
//! `on_error` (on_error.rs); order, first wins, matchers and interrupts
//! (order.rs); the parity guard (parity.rs); what `turn.end` hands a hook
//! (turn_end.rs). That every decision takes the
//! same effect from a `.ts` mod and from a `Hooks` closure is haest-bridges'
//! test, over the testing module's decision cases.

use std::sync::Arc;

use haest_core::{
    Message, NoHooks, Tools,
    error::BoxError,
    testing::{ScriptedProvider, text_answer},
    workspace::WorkspaceMode,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    events::{Event, ModEvent},
    hook::Hook,
    hook_set::Hooks,
    registry::Mods,
    testing::*,
};

mod on_error;
mod order;
mod parity;
mod turn_end;
