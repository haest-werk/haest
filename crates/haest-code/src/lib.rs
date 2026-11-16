//! haest's JavaScript: an embedded QuickJS engine and the two things built on it.
//!
//! - The **Code tool** ([`CodeTool`]): the model writes a short JavaScript
//!   program that calls haest's own tools (`await tools.Grep({...})`), loops,
//!   filters and joins their results, and returns only what matters -- one
//!   model step instead of twenty, and the intermediate results never enter
//!   the context.
//! - The sandbox shell's **`js` command** ([`sandbox_js`]): `js script.ts`
//!   runs a JavaScript or TypeScript file as an ES module, with Node-shaped
//!   `fs`, `path` and `process` over the sandbox's own files.
//!
//! Both run on the same engine ([`vm`]): QuickJS (native, through
//! `rquickjs`) on a thread of its own per run, with a memory limit, a
//! wall-clock deadline that stops even a CPU loop, and no reach of its own --
//! no file system, network, process or timer except what haest hands the run.
//!
//! | Piece | What | Ported from |
//! |---|---|---|
//! | [`vm`] | one engine run on a thread of its own: limits, stop, heartbeat | `core/code-vm*.ts` |
//! | [`call_gate`] | writes alone, reads together, for one run's tool calls | `core/call-gate.ts` |
//! | [`code`] | the Code tool: a short program that calls haest's tools | `tools/code*.ts` |
//! | [`sandbox_js`] | the sandbox shell's `js <file>` command over a haest [`Fs`](haest_core::Fs) | `core/sandbox-js*.ts` |
//!
//! Feature `typescript` (default): `js file.ts` strips the types first
//! (`haest-ts`). Without it, a TypeScript file is refused with a message
//! saying so.
//!
//! # When to use this crate directly
//!
//! The `haest` facade re-exports it as `haest::code` (feature `code`, on by
//! default), and `haest-runtime` adds the Code tool when its `code_tools`
//! setting is on and gives every sandbox the `js` command. Take this crate
//! yourself when you build the agent's tool list or its sandbox by hand, or
//! to run JavaScript of your own on the engine ([`run_in_vm`]).
//!
//! # The Code tool
//!
//! Build it over the names [`code_tool_names`] allows and the same tool list
//! the model gets. If the host wraps its tools (mods, approvals), hand Code the
//! wrapped list: every call a program makes then passes the same checks as a
//! direct call.
//!
//! ```
//! use std::sync::Arc;
//!
//! use haest_code::{CodeTool, code_tool_names};
//! use haest_core::{MemFs, Tool, ToolContext};
//! use haest_tools::{GlobTool, ReadTool};
//! use haest_workspace::SandboxWorkspace;
//! use serde_json::json;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let files = MemFs::with_files([
//!     ("/project/notes/a.md", "TODO: write the intro\n"),
//!     ("/project/notes/b.md", "done\n"),
//!     ("/project/notes/c.md", "TODO: check the numbers\nTODO: ask Sam\n"),
//! ]);
//! let workspace = Arc::new(SandboxWorkspace::over(Arc::new(files)));
//!
//! // The model's tools. Code may call the file tools among them.
//! let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(ReadTool::default()), Arc::new(GlobTool)];
//! let code = CodeTool::new(code_tool_names(&tools, &[]), Arc::new(tools));
//!
//! let program = r#"
//!   const files = (await tools.Glob({ pattern: 'notes/*.md' })).split('\n').filter(Boolean)
//!   let todos = 0
//!   for (const f of files) todos += (await tools.Read({ file_path: f })).split('TODO').length - 1
//!   return { files: files.length, todos }
//! "#;
//! let input = json!({ "code": program }).as_object().cloned().unwrap();
//! let out = code.execute(input, &ToolContext::new("/project", workspace)).await.unwrap();
//! assert!(!out.is_error, "{}", out.content);
//! // The model reads the returned value (and a count of the calls it took),
//! // not the four tool results.
//! assert!(out.content.contains("\"todos\": 3"));
//! # }
//! ```
//!
//! # The sandbox `js` command
//!
//! [`sandbox_commands`] is `js`, plus a pointer to it under the names a model
//! reaches for out of habit (`node`, `bun`, ...). Give each to a sandbox; the
//! script then runs inside the shell call, pipes and all.
//!
//! ```
//! use std::{sync::Arc, time::Duration};
//!
//! use haest_code::sandbox_commands;
//! use haest_core::{ExecOptions, MemFs, Workspace};
//! use haest_workspace::SandboxWorkspace;
//! use tokio_util::sync::CancellationToken;
//!
//! # #[tokio::main]
//! # async fn main() {
//! let files = MemFs::with_files([(
//!     "/project/hi.ts",
//!     "const who: string = process.argv[2]\nconsole.log(`hi ${who}`)",
//! )]);
//! let sandbox = sandbox_commands()
//!     .into_iter()
//!     .fold(SandboxWorkspace::over(Arc::new(files)), SandboxWorkspace::with_command);
//!
//! let opts = ExecOptions {
//!     cwd: "/project".into(),
//!     cancel: CancellationToken::new(),
//!     timeout: Duration::from_secs(20),
//! };
//! let run = sandbox.shell("js hi.ts haest | tr a-z A-Z", opts.clone()).await;
//! assert_eq!((run.stdout.as_str(), run.exit_code), ("HI HAEST\n", 0));
//!
//! let habit = sandbox.shell("node hi.ts", opts).await;
//! assert_eq!(habit.exit_code, 127);
//! assert!(habit.stderr.contains("js <file>"));
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![warn(missing_docs)]

pub mod call_gate;
pub mod code;
pub mod sandbox_js;
#[cfg(test)]
mod testkit;
pub mod vm;

pub use code::{CodeTool, code_callable, code_tool_names};
pub use sandbox_js::{JsEnv, js_command, not_here, run_js, sandbox_commands};
pub use vm::{HostFn, VmBeat, VmLimits, VmRun, VmStop, run_in_vm};
