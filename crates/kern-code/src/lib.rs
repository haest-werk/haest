//! kern's JavaScript: an embedded QuickJS engine ([`vm`]) and the two things built on it.
//!
//! | Piece | What | Ported from |
//! |---|---|---|
//! | [`vm`] | one engine run on a thread of its own: limits, stop, heartbeat | `core/code-vm*.ts` |
//! | [`call_gate`] | writes alone, reads together, for one run's tool calls | `core/call-gate.ts` |
//! | [`code`] | the Code tool: a short program that calls kern's tools | `tools/code*.ts` |
//! | [`sandbox_js`] | the sandbox shell's `js <file>` command over a kern [`kern_core::Fs`] | `core/sandbox-js*.ts` |

pub mod call_gate;
pub mod code;
#[cfg(test)]
mod testkit;
pub mod vm;

pub use code::{CodeTool, code_callable, code_tool_names};
pub use vm::{HostFn, VmBeat, VmLimits, VmRun, VmSpec, VmStop, run_in_vm};
