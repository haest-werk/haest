//! `js` and the runtime names that point at it, as haest [`ShellCommand`]s: what
//! a sandbox shell registers ([`sandbox_commands`]).

use std::sync::Arc;

use async_trait::async_trait;
use haest_core::{
    shell_command::{ShellCall, ShellCommand},
    workspace::ExecResult,
};

use super::{JS_COMMAND, JsEnv, NOT_HERE, js_command, not_here};

/// What the model is told the sandbox can run, beside the shell itself.
#[cfg(feature = "typescript")]
const JS_NOTE: &str = "To run a script you wrote, use `js <file.js|file.ts> [args]`: plain JavaScript/TypeScript as an ES module with async fs (fs.promises / 'fs/promises'), path and process over the sandbox's files -- no npm packages, no sync fs.";
#[cfg(not(feature = "typescript"))]
const JS_NOTE: &str = "To run a script you wrote, use `js <file.js> [args]`: plain JavaScript as an ES module with async fs (fs.promises / 'fs/promises'), path and process over the sandbox's files -- no npm packages, no sync fs, no TypeScript.";

/// `js` and, for every name in [`NOT_HERE`](crate::NOT_HERE), a command that
/// points to it, ready for `SandboxWorkspace::with_command`. Only `js` has a
/// note for the system prompt; the others stay unadvertised.
pub fn sandbox_commands() -> Vec<Arc<dyn ShellCommand>> {
    let mut out: Vec<Arc<dyn ShellCommand>> = vec![Arc::new(JsShellCommand)];
    out.extend(
        NOT_HERE
            .iter()
            .map(|name| Arc::new(NotHere(name)) as Arc<dyn ShellCommand>),
    );
    out
}

/// The sandbox shell's `js <file> [args]` command: [`js_command`] over the
/// shell call's files, working directory, variables, interrupt and deadline.
pub(crate) struct JsShellCommand;

#[async_trait]
impl ShellCommand for JsShellCommand {
    fn name(&self) -> &str {
        JS_COMMAND
    }

    fn note(&self) -> Option<String> {
        Some(JS_NOTE.into())
    }

    async fn run(&self, call: ShellCall) -> ExecResult {
        let stdin = call.stdin_text().map(|text| text.into_owned());
        let env = JsEnv {
            fs: call.fs,
            cwd: call.cwd,
            env: call.env,
            stdin,
            cancel: call.cancel,
            ends_at: Some(call.ends_at),
        };
        js_command(&call.args, &env).await
    }
}

/// A runtime name (`bun`, `node`, ...) that answers with a pointer to `js`
/// and exit 127 ([`not_here`]); the field is the name.
pub(crate) struct NotHere(pub &'static str);

#[async_trait]
impl ShellCommand for NotHere {
    fn name(&self) -> &str {
        self.0
    }

    async fn run(&self, _call: ShellCall) -> ExecResult {
        not_here(self.0)
    }
}
