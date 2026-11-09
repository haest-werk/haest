//! `js <file> [args]` in the SANDBOX shell: runs a JavaScript or TypeScript file
//! in kern's QuickJS engine ([`crate::vm`], the one the Code tool uses), as an ES
//! module, with Node-shaped fs / path / process over the sandbox's own file
//! system ([`guest`]). Output and exit codes behave like a CLI: console.log ->
//! stdout, console.error -> stderr, an uncaught error -> `✗ Uncaught ...` and exit 1.
//!
//! Deliberately NOT called `bun` or `node`: it is neither -- no npm packages, no
//! sync fs, no network. Those names answer with a pointer here instead
//! ([`not_here`]), so a model that reaches for `bun script.ts` out of habit learns
//! the real command in one step. The real workspace has the real runtimes; none
//! of this belongs there.
//!
//! No tools from here: a script runs inside a Bash call, below the tool layer, so
//! its calls could not be gated or shown. The Code tool is the way to call tools.
//!
//! # Wiring it into a sandbox shell
//!
//! This crate knows no shell. A sandbox registers one builtin per name -- [`JS_COMMAND`]
//! and each of [`NOT_HERE`] -- whose body is one call. For a bashkit `Builtin`:
//!
//! ```text
//! async fn execute(&self, ctx: bashkit::Context<'_>) -> bashkit::Result<ExecResult> {
//!     let env = JsEnv {
//!         fs: self.kern_fs.clone(),                  // the kern Fs the sandbox's bashkit fs adapts
//!         cwd: ctx.cwd.clone(),
//!         env: ctx.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
//!         cancel: self.call_cancel(),                // the running shell call's interrupt
//!         ends_at: self.call_ends_at(),              // the running shell call's deadline (Instant)
//!     };
//!     let out = js_command(ctx.args, &env).await;    // or not_here(name) for bun, node, ...
//!     Ok(ExecResult { stdout: out.stdout, stderr: out.stderr, exit_code: out.exit_code, .. })
//! }
//! ```
//!
//! `ends_at` matters: a CPU-bound script never yields, so the engine checks the
//! shell call's wall-clock deadline itself -- the only bound such a script cannot outrun.

mod guest;
#[cfg(test)]
mod tests;
#[cfg(feature = "typescript")]
mod typescript;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use kern_core::{fs::Fs, tool::resolve_path};
use tokio_util::sync::CancellationToken;

pub use guest::JS_MODULES;

use crate::vm::{VM_MEMORY_BYTES, VmLimits, VmRun, VmStop, run_in_vm};
use guest::{JsScript, js_script_spec};

/// The command's name in the sandbox shell.
pub const JS_COMMAND: &str = "js";
/// Runtime names that answer with a pointer to `js` instead of pretending.
pub const NOT_HERE: [&str; 7] = ["bun", "bunx", "node", "npx", "deno", "tsx", "ts-node"];
/// The shell call's own deadline is the real bound; this caps a script with none.
pub const JS_TIMEOUT: Duration = Duration::from_secs(300);
pub const JS_MAX_OUTPUT: usize = 1_000_000;

/// Where a `js` command runs: the sandbox's files and the shell call it is part of.
#[derive(Clone)]
pub struct JsEnv {
    pub fs: Arc<dyn Fs>,
    pub cwd: PathBuf,
    /// The shell's exported variables: the script's `process.env`.
    pub env: BTreeMap<String, String>,
    /// The shell call's interrupt.
    pub cancel: CancellationToken,
    /// The running shell call's end; None = [`JS_TIMEOUT`] from now.
    pub ends_at: Option<Instant>,
}

/// What a shell gets back from a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

fn output(stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) -> JsOutput {
    JsOutput {
        stdout: stdout.into(),
        stderr: stderr.into(),
        exit_code,
    }
}

fn usage() -> String {
    format!(
        "usage: js <file.js|file.ts> [args...]
Runs a JavaScript or TypeScript file in the sandbox's JS engine, as an ES module (top-level await works).
Modules: {} (import or require; node: prefix ok). The fs API is async only -- fs.promises / 'fs/promises'.
No npm packages, no network. To call kern's tools from code, use the Code tool.
",
        JS_MODULES.join(", ")
    )
}

/// What `bun`, `node`, ... answer in the sandbox: a pointer to `js`, exit 127.
pub fn not_here(name: &str) -> JsOutput {
    output(
        "",
        format!(
            "{name}: not available in this sandbox. Run JavaScript or TypeScript with: js <file> [args]  (async fs + path, no npm packages). To call kern's tools from code, use the Code tool.\n"
        ),
        127,
    )
}

fn is_typescript(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(ext, "ts" | "mts" | "cts" | "tsx" | "mtsx" | "ctsx")
}

#[cfg(feature = "typescript")]
fn runnable(source: String, path: &Path) -> Result<String, String> {
    if !is_typescript(path) {
        return Ok(source);
    }
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    typescript::strip_types(&source, &name)
}

#[cfg(not(feature = "typescript"))]
fn runnable(source: String, path: &Path) -> Result<String, String> {
    if is_typescript(path) {
        return Err("TypeScript is not supported in this build; write JavaScript".into());
    }
    Ok(source)
}

/// `js <file> [args]` -- `args` are the command's arguments, not including `js` itself.
pub async fn js_command(args: &[String], env: &JsEnv) -> JsOutput {
    let Some(file) = args.first() else {
        return output("", usage(), 2);
    };
    if file == "-h" || file == "--help" {
        return output(usage(), "", 0);
    }
    let path = resolve_path(&env.cwd, file);
    if !env.fs.exists(&path).await {
        return output("", format!("js: {file}: no such file\n"), 1);
    }
    let source = match env.fs.read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => return output("", format!("js: {file}: {e}\n"), 1),
    };
    match runnable(source, &path) {
        Ok(code) => run_js(&path, &code, &args[1..], env).await,
        Err(e) => output("", format!("js: {file}: {e}\n"), 1),
    }
}

/// Run JavaScript already in hand, as the module at `path` (absolute: `process.argv[1]`,
/// and its file name in error lines), with `args` after it in argv.
pub async fn run_js(path: &Path, code: &str, args: &[String], env: &JsEnv) -> JsOutput {
    let exit_code = Arc::new(Mutex::new(None));
    let mut argv = vec![JS_COMMAND.to_string(), path.display().to_string()];
    argv.extend(args.iter().cloned());
    let script = JsScript {
        fs: env.fs.clone(),
        cwd: env.cwd.clone(),
        argv,
        env: env.env.clone(),
        exit_code: exit_code.clone(),
    };
    let left = env.ends_at.map_or(JS_TIMEOUT, |end| {
        end.saturating_duration_since(Instant::now())
    });
    let timeout = left.min(JS_TIMEOUT);
    let limits = VmLimits {
        timeout_ms: timeout.as_millis() as u64,
        memory_bytes: VM_MEMORY_BYTES,
        max_log_chars: JS_MAX_OUTPUT,
    };
    let filename = path
        .file_name()
        .map_or_else(|| "script.js".into(), |n| n.to_string_lossy().into_owned());
    let spec = js_script_spec(script, &filename);
    let run = run_in_vm(code, &spec, limits, &env.cancel, None).await;
    let exited = *exit_code.lock().unwrap();
    cli_result(run, exited, timeout)
}

/// A run as a CLI result: the script's own exit code wins, then failure = 1.
fn cli_result(run: VmRun, exit_code: Option<i32>, timeout: Duration) -> JsOutput {
    let uncaught = match (&run.error, exit_code) {
        (Some(e), None) => format!("✗ Uncaught {e}\n"),
        _ => String::new(),
    };
    let stopped = match run.stopped {
        Some(VmStop::Timeout) => format!(
            "js: stopped (ran out its {}s)\n",
            (timeout.as_millis() as f64 / 1000.0).round()
        ),
        Some(VmStop::Interrupted) => "js: stopped (interrupted)\n".into(),
        None => String::new(),
    };
    let truncated = if run.logs_truncated {
        format!("js: output truncated at {JS_MAX_OUTPUT} characters\n")
    } else {
        String::new()
    };
    let failed = !uncaught.is_empty() || !stopped.is_empty();
    let code = exit_code.unwrap_or(i32::from(failed));
    output(
        run.stdout,
        format!("{}{uncaught}{stopped}{truncated}", run.stderr),
        code,
    )
}
