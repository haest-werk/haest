//! kern's JavaScript: an embedded QuickJS engine ([`vm`]) and the two things built on it.
//!
//! | Piece | What | Ported from |
//! |---|---|---|
//! | [`vm`] | one engine run on a thread of its own: limits, stop, heartbeat | `core/code-vm*.ts` |
//! | [`call_gate`] | writes alone, reads together, for one run's tool calls | `core/call-gate.ts` |
//! | [`code`] | the Code tool: a short program that calls kern's tools | `tools/code*.ts` |
//! | [`sandbox_js`] | the sandbox shell's `js <file>` command over a kern [`kern_core::Fs`] | `core/sandbox-js*.ts` |
//!
//! Feature `typescript` (default): `js file.ts` strips the types first. Without
//! it, a TypeScript file is refused with a message saying so.
//!
//! Wiring, for a host: build the Code tool with [`CodeTool::new`] over the
//! names [`code_tool_names`] allows and the same tool list the model gets; give a
//! sandbox shell [`js_command`] and [`not_here`] as builtins (see [`sandbox_js`]).

pub mod call_gate;
pub mod code;
pub mod sandbox_js;
#[cfg(test)]
mod testkit;
pub mod vm;

pub use code::{CodeTool, code_callable, code_tool_names};
pub use sandbox_js::{JsEnv, JsOutput, js_command, not_here, run_js};
pub use vm::{HostFn, VmBeat, VmLimits, VmRun, VmSpec, VmStop, run_in_vm};
