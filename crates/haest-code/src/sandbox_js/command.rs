//! `js` and the runtime names that point at it, as haest [`ShellCommand`]s: what
//! a sandbox shell registers ([`sandbox_commands`]).

use async_trait::async_trait;
use haest_core::{
    ChildEnv,
    shell_command::{ShellCall, ShellCommand, ShellCommandRef},
    workspace::ExecResult,
};

use super::{JS_COMMAND, JsEnv, NOT_HERE, js_command, not_here};
use crate::vm::VmCaps;

/// What the model is told the sandbox can run, beside the shell itself.
#[cfg(feature = "typescript")]
const JS_NOTE: &str = "To run a script you wrote, use `js <file.js|file.ts> [args]`: plain JavaScript/TypeScript as an ES module with async fs (fs.promises / 'fs/promises'), path and process over the sandbox's files -- no npm packages, no sync fs.";
#[cfg(not(feature = "typescript"))]
const JS_NOTE: &str = "To run a script you wrote, use `js <file.js> [args]`: plain JavaScript as an ES module with async fs (fs.promises / 'fs/promises'), path and process over the sandbox's files -- no npm packages, no sync fs, no TypeScript.";

/// `js` and, for every name in [`NOT_HERE`](crate::NOT_HERE), a command that
/// points to it, ready for `SandboxWorkspace::with_commands`. Only `js` has a
/// note for the system prompt; the others stay unadvertised. A script runs
/// with [`VmCaps::sandbox`]; [`sandbox_commands_with`] takes other caps.
pub fn sandbox_commands() -> Vec<ShellCommandRef> {
    sandbox_commands_with(VmCaps::sandbox())
}

/// [`sandbox_commands`] with `js` held to `caps`: its memory, its output,
/// and the longest a script runs (the shell call's end still cuts it
/// shorter).
///
/// ```
/// use std::time::Duration;
///
/// use haest_code::{VmCaps, sandbox_commands_with};
///
/// let patient = VmCaps::sandbox().with_timeout(Duration::from_secs(20 * 60));
/// let commands = sandbox_commands_with(patient);
/// assert_eq!(commands[0].name(), "js");
/// ```
pub fn sandbox_commands_with(caps: VmCaps) -> Vec<ShellCommandRef> {
    let js = ShellCommandRef::new(JsShellCommand { caps });
    let pointers = NOT_HERE
        .iter()
        .map(|name| ShellCommandRef::new(NotHere(name)));
    std::iter::once(js).chain(pointers).collect()
}

/// The sandbox shell's `js <file> [args]` command: [`js_command`] over the
/// shell call's files, working directory, variables, interrupt and
/// deadline, held to `caps`.
pub(crate) struct JsShellCommand {
    caps: VmCaps,
}

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
            env: ChildEnv::new(call.env),
            stdin,
            cancel: call.cancel,
            ends_at: Some(call.ends_at),
            caps: self.caps,
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
