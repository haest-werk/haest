//! A host's [`ShellCommand`]s as bashkit builtins: one adapter per command per
//! shell call, carrying that call's interrupt and deadline.

use std::{sync::Arc, time::Instant};

use bashkit::{Builtin, BuiltinContext};
use haest_core::{
    fs::Fs,
    shell_command::{ShellCall, ShellCommand},
};
use tokio_util::sync::CancellationToken;

pub(crate) struct HostCommand {
    pub command: Arc<dyn ShellCommand>,
    pub fs: Arc<dyn Fs>,
    pub cancel: CancellationToken,
    pub ends_at: Instant,
}

#[async_trait::async_trait]
impl Builtin for HostCommand {
    async fn execute(&self, ctx: BuiltinContext<'_>) -> bashkit::Result<bashkit::ExecResult> {
        let env = ctx.env.iter().map(|(k, v)| (k.clone(), v.clone()));
        let mut call = ShellCall::new(
            ctx.args.iter().cloned(),
            ctx.cwd.clone(),
            self.fs.clone(),
            self.ends_at,
        )
        .with_env(env.collect())
        .with_cancel(self.cancel.clone());
        if let Some(stdin) = ctx.stdin {
            call = call.with_stdin(stdin.as_bytes());
        }
        let out = self.command.run(call).await;
        let mut result = bashkit::ExecResult::with_code(out.stdout, out.exit_code);
        result.stderr = out.stderr.into();
        Ok(result)
    }
}
