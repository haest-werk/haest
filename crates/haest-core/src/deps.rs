//! What the loop needs ([`AgentDeps`]), the host's optional say in it
//! ([`AgentHooks`]), and the clock its time cap reads ([`Clock`]).
//!
//! [`SharedDeps::new`] takes what every agent of a conversation runs on: a
//! [`ModelResolver`](crate::ModelResolver), a working directory and a
//! [`Workspace`](crate::Workspace). [`AgentDeps::new`] takes that and the
//! agent's own [`ToolSource`](crate::ToolSource) and [`SystemPrompt`], each
//! as a plain value. Everything else is off until the host sets it with a
//! `with_*` setter ([`AgentDeps::with_hooks`], [`SharedDeps::with_ledger`],
//! ...). [`Agent::new`](crate::Agent::new) takes the result.
//!
//! Every plug point is held by a handle named after its trait
//! ([`AgentHooksRef`], [`SystemPromptRef`], ...): cheap to clone, and built
//! from the value itself, or from an `Arc` the host already shares, so one
//! object can fill several slots.
//!
//! PLUG POINT: [`AgentHooks`] is where a host changes the loop's behaviour
//! without forking it. Every method has a default that does nothing, so a host
//! implements only what it cares about. [`NoHooks`] is the one that changes
//! nothing.

mod agent_deps;
mod hooks;
mod shared;

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

pub use agent_deps::AgentDeps;
pub use hooks::{AgentHooks, AgentHooksRef, NoHooks};
pub use shared::SharedDeps;

/// The system prompt, built again before every model call (instruction files
/// and skills can change mid-conversation).
///
/// A `String` or a `&'static str` is a fixed prompt, and a closure
/// `Fn() -> String` builds one each call. A host that assembles one from
/// files and skills implements this, typically with
/// [`build_system_prompt`](crate::context::build_system_prompt).
#[async_trait]
pub trait SystemPrompt: Send + Sync {
    /// The prompt for the next model call, and for compaction's summary
    /// request (which must send the same one to hit the prompt cache).
    async fn system_prompt(&self) -> String;
}

/// A fixed prompt: the string itself, every call.
#[async_trait]
impl SystemPrompt for String {
    async fn system_prompt(&self) -> String {
        self.clone()
    }
}

/// A fixed prompt: the string itself, every call.
#[async_trait]
impl SystemPrompt for &'static str {
    async fn system_prompt(&self) -> String {
        (*self).to_owned()
    }
}

/// A prompt the closure builds, every call.
#[async_trait]
impl<F: Fn() -> String + Send + Sync> SystemPrompt for F {
    async fn system_prompt(&self) -> String {
        self()
    }
}

crate::plug_ref! {
    /// A [`SystemPrompt`], shared: what [`AgentDeps`] holds. Built from the
    /// prompt itself (`"You are terse."`, a `String`, a closure, a host's
    /// own type), or from an `Arc` of one.
    pub struct SystemPromptRef(dyn SystemPrompt);
}

/// Milliseconds since the Unix epoch, by the wall clock; 0 if the clock is
/// set before 1970. What [`WallClock`] reads.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// PLUG POINT: the clock the turn guard's time cap reads, in milliseconds
/// since the epoch ([`SharedDeps::with_clock`]). A host that pauses runs
/// gives one that stands still while paused. A closure `Fn() -> u64` is a
/// clock.
pub trait Clock: Send + Sync {
    /// Now, in milliseconds since the epoch. Default: the wall clock
    /// ([`now_ms`]), so a host object that fills every slot
    /// ([`SharedDeps::with_host`]) keeps it with an empty `impl`.
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

impl<F: Fn() -> u64 + Send + Sync> Clock for F {
    fn now_ms(&self) -> u64 {
        self()
    }
}

crate::plug_ref! {
    /// A [`Clock`], shared: what [`SharedDeps::clock`] holds.
    pub struct ClockRef(dyn Clock);
}

/// The wall clock ([`now_ms`]): [`AgentDeps::new`]'s default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        now_ms()
    }
}

#[cfg(test)]
mod tests;
