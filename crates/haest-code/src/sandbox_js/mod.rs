//! `js <file> [args]` in the SANDBOX shell: runs a JavaScript or TypeScript file
//! in haest's QuickJS engine ([`crate::vm`], the one the Code tool uses), as an ES
//! module, with Node-shaped fs / path / process over the sandbox's own file
//! system. Output and exit codes behave like a CLI: console.log ->
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
//! # What a script has
//!
//! | Name | What |
//! |---|---|
//! | `fs`, `fs/promises` | `readFile`, `writeFile`, `appendFile`, `readdir`, `mkdir` (`{ recursive }`), `stat`, `rm` (`{ recursive, force }`), `unlink`, `rename`, `copyFile`, `access`: async only, text only |
//! | `path` | Node's `path.posix`: `join`, `resolve`, `dirname`, `basename`, `extname`, `relative`, `normalize`, `isAbsolute`, `sep`, `delimiter` |
//! | `process` | `argv` (`['js', '/abs/script.js', ...args]`), `env` (the shell's exported variables), `cwd()`, `exit(code)`, `stdin`, `stdout.write`, `stderr.write` |
//! | `console` | `log`, `info` and `debug` to stdout; `warn` and `error` to stderr |
//!
//! The modules ([`JS_MODULES`]) are imported (`import { readFile } from
//! 'fs/promises'`, a `node:` prefix works too) or `require`d; importing
//! anything else fails the run, and `require` of it throws. Every `*Sync`
//! name exists and throws, naming the async call to use instead. Paths resolve against the shell's working directory, over
//! the sandbox's files: in the default sandbox, writes land in its
//! copy-on-write overlay, not on disk. Top-level `await` works. What is
//! piped into `js` is the script's stdin, all there when it starts:
//! `for await (const chunk of process.stdin)`, `process.stdin.on('data')` /
//! `on('end')`, or `readFile('/dev/stdin')`.
//!
//! # Wiring it into a sandbox shell
//!
//! This crate knows no shell. [`sandbox_commands`] is `js` and each of
//! [`NOT_HERE`] as haest-core [`haest_core::shell_command::ShellCommand`]s; a
//! sandbox takes each one (`SandboxWorkspace::with_command`), and `js`'s note
//! tells the model it is there. The [crate docs](crate#the-sandbox-js-command)
//! wire one up. Without a shell, [`js_command`] runs the same command over
//! any [`Fs`].
//!
//! The shell call's deadline matters: a CPU-bound script never yields, so the
//! engine checks that wall-clock deadline itself -- the only bound such a script cannot outrun.

mod command;
mod guest;
mod source;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use haest_core::{
    fs::{Fs, resolve_path},
    workspace::ExecResult,
};
use tokio_util::sync::CancellationToken;

pub use command::{JsShellCommand, NotHere, sandbox_commands};
pub use guest::JS_MODULES;

use crate::vm::{VmCaps, VmRun, VmStop, run_in_vm};
use guest::{JsScript, js_script_spec};
use source::{is_jsx, runnable};

/// The command's name in the sandbox shell.
pub const JS_COMMAND: &str = "js";
/// Runtime names that answer with a pointer to `js` instead of pretending.
pub const NOT_HERE: [&str; 7] = ["bun", "bunx", "node", "npx", "deno", "tsx", "ts-node"];
/// The longest a script runs (5 minutes). The shell call's own deadline is the
/// real bound; this caps a script with none, and one with a later one.
pub const JS_TIMEOUT: Duration = Duration::from_secs(300);
/// The most console output a script may print, in characters, stdout and
/// stderr together; the rest is dropped and stderr says so.
pub const JS_MAX_OUTPUT: usize = 1_000_000;

/// Where a `js` command runs: the sandbox's files and the shell call it is part of.
#[derive(Clone)]
pub struct JsEnv {
    /// The files the script's `fs` reads and writes: the sandbox's own.
    pub fs: Arc<dyn Fs>,
    /// The shell's working directory: relative paths, `process.cwd()` and the script's own path resolve against it.
    pub cwd: PathBuf,
    /// The shell's exported variables: the script's `process.env`.
    pub env: BTreeMap<String, String>,
    /// What is piped into the command (`process.stdin`, `/dev/stdin`); None =
    /// nothing.
    pub stdin: Option<String>,
    /// The shell call's interrupt.
    pub cancel: CancellationToken,
    /// The running shell call's end; None = [`JS_TIMEOUT`] from now.
    pub ends_at: Option<Instant>,
}

fn output(stdout: impl Into<String>, stderr: impl Into<String>, exit_code: i32) -> ExecResult {
    ExecResult {
        stdout: stdout.into(),
        stderr: stderr.into(),
        exit_code,
        timed_out: false,
    }
}

fn usage() -> String {
    format!(
        "usage: js <file.js|file.ts> [args...]
Runs a JavaScript or TypeScript file in the sandbox's JS engine, as an ES module (top-level await works).
Modules: {} (import or require; node: prefix ok). The fs API is async only -- fs.promises / 'fs/promises'.
No npm packages, no network. To call haest's tools from code, use the Code tool.
",
        JS_MODULES.join(", ")
    )
}

/// What `bun`, `node`, ... answer in the sandbox: a pointer to `js` on
/// stderr, exit 127 (the shell's "command not found").
pub fn not_here(name: &str) -> ExecResult {
    output(
        "",
        format!(
            "{name}: not available in this sandbox. Run JavaScript or TypeScript with: js <file> [args]  (async fs + path, no npm packages). To call haest's tools from code, use the Code tool.\n"
        ),
        127,
    )
}

/// Runs `js <file> [args]` as the shell would: reads the file from `env.fs`,
/// strips its types when it is TypeScript, and runs it with [`run_js`].
///
/// `args` are the command's arguments, not including `js` itself. The answer
/// is a CLI's: no file is a usage message and exit 2, `-h` / `--help` the
/// usage on stdout and exit 0, a file that is missing, unreadable or not
/// valid TypeScript exit 1 with the reason on stderr. A file is TypeScript by
/// its extension: `.ts`, `.mts`, `.cts`; without the `typescript` feature
/// such a file is refused. A JSX file (`.tsx`, `.mtsx`, `.ctsx`, `.jsx`) is
/// refused, exit 1: the engine runs no JSX.
///
/// `env.cancel` stops the script; so does the shell call's deadline,
/// `env.ends_at`.
///
/// # Examples
///
/// ```
/// use std::{path::Path, sync::Arc, time::{Duration, Instant}};
///
/// use haest_code::{JsEnv, js_command};
/// use haest_core::{Fs, MemFs};
/// use tokio_util::sync::CancellationToken;
///
/// # #[tokio::main]
/// # async fn main() {
/// let fs = Arc::new(MemFs::with_files([(
///     "/work/sum.js",
///     "import { readFile, writeFile } from 'fs/promises'
///      const nums = (await readFile('nums.txt', 'utf8')).trim().split('\\n').map(Number)
///      await writeFile('total.txt', String(nums.reduce((a, b) => a + b, 0)))",
/// ), ("/work/nums.txt", "1\n2\n3\n")]));
/// let env = JsEnv {
///     fs: fs.clone(),
///     cwd: "/work".into(),
///     env: Default::default(),
///     stdin: None,
///     cancel: CancellationToken::new(),
///     ends_at: Some(Instant::now() + Duration::from_secs(20)),
/// };
/// let run = js_command(&["sum.js".to_string()], &env).await;
/// assert_eq!(run.exit_code, 0, "{}", run.stderr);
/// assert_eq!(fs.read_to_string(Path::new("/work/total.txt")).await.unwrap(), "6");
///
/// assert_eq!(js_command(&["missing.js".to_string()], &env).await.exit_code, 1);
/// # }
/// ```
pub async fn js_command(args: &[String], env: &JsEnv) -> ExecResult {
    let Some(file) = args.first() else {
        return output("", usage(), 2);
    };
    if file == "-h" || file == "--help" {
        return output(usage(), "", 0);
    }
    let path = resolve_path(&env.cwd, file);
    if is_jsx(&path) {
        let why = "JSX does not run here (the engine has no JSX); write .ts or .js";
        return output("", format!("js: {file}: {why}\n"), 1);
    }
    if !env.fs.exists(&path).await {
        return output("", format!("js: {file}: no such file\n"), 1);
    }
    let source = match env.fs.read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => return output("", format!("js: {file}: {e}\n"), 1),
    };
    match runnable(source, &path).await {
        Ok(code) => run_js(&path, &code, &args[1..], env).await,
        Err(e) => output("", format!("js: {file}: {e}\n"), 1),
    }
}

/// Runs JavaScript already in hand, as the module at `path` (absolute:
/// `process.argv[1]`, and its file name in error lines), with `args` after it
/// in argv. Nothing is read from `path`; `code` must already be JavaScript.
///
/// The run gets the script API in the [module docs](self#what-a-script-has),
/// [`VM_MEMORY_BYTES`](crate::vm::VM_MEMORY_BYTES) of memory, and until
/// `env.ends_at` (at most [`JS_TIMEOUT`]). The result is a CLI's: what it
/// printed, and the exit code it passed to `process.exit`, else 1 when it
/// threw or was stopped, else 0. An uncaught error, a stop (`env.cancel` or
/// the deadline) and output past [`JS_MAX_OUTPUT`] each add a line to stderr.
pub async fn run_js(path: &Path, code: &str, args: &[String], env: &JsEnv) -> ExecResult {
    let exit_code = Arc::new(Mutex::new(None));
    let mut argv = vec![JS_COMMAND.to_string(), path.display().to_string()];
    argv.extend(args.iter().cloned());
    let script = JsScript {
        fs: env.fs.clone(),
        cwd: env.cwd.clone(),
        argv,
        env: env.env.clone(),
        stdin: env.stdin.clone(),
        exit_code: exit_code.clone(),
    };
    let left = env.ends_at.map_or(JS_TIMEOUT, |end| {
        end.saturating_duration_since(Instant::now())
    });
    let timeout = left.min(JS_TIMEOUT);
    let caps = VmCaps {
        timeout_ms: timeout.as_millis() as u64,
        max_log_chars: JS_MAX_OUTPUT,
        ..VmCaps::default()
    };
    let filename = path
        .file_name()
        .map_or_else(|| "script.js".into(), |n| n.to_string_lossy().into_owned());
    let spec = js_script_spec(script, &filename);
    let run = run_in_vm(code, &spec, caps, &env.cancel, None).await;
    let exited = *exit_code.lock().unwrap();
    cli_result(run, exited, timeout)
}

/// A run as a CLI result: the script's own exit code wins, then failure = 1.
fn cli_result(run: VmRun, exit_code: Option<i32>, timeout: Duration) -> ExecResult {
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
