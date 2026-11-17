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
        let call = ShellCall {
            args: ctx.args.to_vec(),
            cwd: ctx.cwd.clone(),
            env: ctx
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            stdin: ctx.stdin.map(|s| s.as_bytes().to_vec()),
            fs: self.fs.clone().into(),
            cancel: self.cancel.clone(),
            ends_at: self.ends_at,
        };
        let out = self.command.run(call).await;
        let mut result = bashkit::ExecResult::with_code(out.stdout, out.exit_code);
        result.stderr = out.stderr.into();
        Ok(result)
    }
}
