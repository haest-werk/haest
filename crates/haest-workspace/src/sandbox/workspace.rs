//! The sandbox as a haest [`Workspace`]: its shell and argv calls, the note
//! the system prompt carries about it, and the changes to review.

use async_trait::async_trait;
use haest_core::{
    fs::FsRef,
    remedy::Limit,
    workspace::{ChangeSetRef, ExecOptions, ExecResult, Workspace, WorkspaceMode},
};

use super::{SandboxWorkspace, exec};

/// What the shell is, for every sandbox. Each added command's own note follows it.
const SHELL_NOTE: &str = "Bash is a simulated shell: coreutils, grep, rg, sed, awk, jq, find, diff, tar and friends, but NO git, bun, node, npm, python or network, and no other real programs (for a web page, use the WebFetch tool -- it runs outside the sandbox).";

/// Where edits go in the default sandbox, and how they reach the disk.
const OVERLAY_NOTE: &str = "Reads come from the real project; every file you write or edit lives in memory until the human reviews it and puts it on disk -- otherwise it is discarded when this conversation ends. When you finish edits worth keeping, tell the human they are waiting for review. If the task needs real builds, tests or git, say so plainly.";

/// `"$@"` runs the positional parameters as one command: argv, no shell parsing.
const RUN_ARGV: &str = "\"$@\"";

#[async_trait]
impl Workspace for SandboxWorkspace {
    fn mode(&self) -> WorkspaceMode {
        WorkspaceMode::Sandbox
    }

    fn fs(&self) -> FsRef {
        self.fs.clone()
    }

    async fn shell(&self, command: &str, opts: ExecOptions) -> ExecResult {
        exec::exec(self.shell_world(), command, &[], opts).await
    }

    async fn run(&self, argv: &[String], opts: ExecOptions) -> ExecResult {
        if argv.first().is_none_or(String::is_empty) {
            return crate::no_command();
        }
        exec::exec(self.shell_world(), RUN_ARGV, argv, opts).await
    }

    fn prompt_note(&self) -> String {
        if let Some(note) = &self.note {
            return note.clone();
        }
        let mut parts = vec![SHELL_NOTE.to_string()];
        parts.extend(self.commands.iter().filter_map(|c| c.note()));
        if self.changes.is_some() {
            parts.push(self.remedies.add_to(OVERLAY_NOTE, Limit::SandboxEdits));
        }
        format!("# Sandbox\nYou run in a SANDBOX. {}", parts.join(" "))
    }

    fn changes(&self) -> Option<ChangeSetRef> {
        self.changes.clone()
    }
}
