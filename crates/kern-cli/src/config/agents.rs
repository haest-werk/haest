//! Turns the human did not start, and subagents, named and defaulted as the
//! TypeScript kern's (`host/limits-config.ts`). Garbage is refused, never
//! silently uncapped.
//!   KERN_MAX_AGENT_TURNS   turns in a row started by a job or an agent with no human
//!                          message between; past it kern HOLDS them (default 30, 0 = no cap)
//!   KERN_MAX_AGENT_DEPTH   how deep agents may nest, main = 0 (default 2)
//!   KERN_MAX_SUBAGENTS     subagents running at once, across the tree (default 4)
//!   KERN_SUBAGENT_USD      one subagent's spend (default 1)
//!   KERN_SUBAGENT_STEPS    one subagent's model calls (default 50)
//!   KERN_SUBAGENT_MINUTES  one subagent's wall clock (default 15)
//! The depth and concurrency caps are NOT "0 = off": 0 means no Agent tool at all.

use kern::tools::agents::SubagentCaps;

use super::{Env, cap};

pub fn max_agent_turns(env: &Env) -> Result<u32, String> {
    Ok(cap(env, "KERN_MAX_AGENT_TURNS", 30.0)? as u32)
}

pub fn subagent_caps(env: &Env) -> Result<SubagentCaps, String> {
    let d = SubagentCaps::default();
    Ok(SubagentCaps {
        max_depth: cap(env, "KERN_MAX_AGENT_DEPTH", d.max_depth.into())? as u32,
        max_concurrent: cap(env, "KERN_MAX_SUBAGENTS", d.max_concurrent as f64)? as usize,
        usd: cap(env, "KERN_SUBAGENT_USD", d.usd)?,
        steps: cap(env, "KERN_SUBAGENT_STEPS", d.steps.into())? as u32,
        minutes: cap(env, "KERN_SUBAGENT_MINUTES", d.minutes as f64)? as u64,
    })
}
