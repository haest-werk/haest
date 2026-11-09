//! The command line. Every flag has a `KERN_*` environment twin (the TS kern's
//! names); a flag wins over its variable.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "kern",
    version,
    about = "An agent in your terminal: a model, tools, a workspace."
)]
pub struct Args {
    /// What to ask. Without it (and with a terminal on stdin) kern starts a conversation.
    pub prompt: Vec<String>,

    /// Answer once and exit; reads the prompt from stdin when none is given.
    #[arg(short, long)]
    pub print: bool,

    /// Write every loop event as one JSON line on stdout (implies --print).
    #[arg(long)]
    pub json: bool,

    /// `provider:model` [KERN_MODEL].
    #[arg(short, long)]
    pub model: Option<String>,

    /// Reasoning effort: off, low, medium, high, xhigh, max [KERN_EFFORT].
    #[arg(short, long)]
    pub effort: Option<String>,

    /// sandbox (default: a simulated bash over a copy-on-write overlay; nothing reaches the disk
    /// until /apply) or real (the real disk and the real bash) [KERN_WORKSPACE].
    #[arg(short, long)]
    pub workspace: Option<String>,

    /// Continue a session: its id, or the latest session in this directory when no id is given.
    #[arg(short, long, num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,

    /// Work in this directory instead of the current one.
    #[arg(short = 'C', long)]
    pub cwd: Option<PathBuf>,

    /// Text appended to the system prompt.
    #[arg(long)]
    pub append_system_prompt: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List the models kern knows, with their windows and prices.
    Models,
    /// List saved sessions, newest first.
    Sessions {
        /// Only sessions started in this directory.
        #[arg(long)]
        here: bool,
    },
}

impl Args {
    /// The prompt words joined, or None.
    pub fn prompt_text(&self) -> Option<String> {
        let text = self.prompt.join(" ");
        (!text.trim().is_empty()).then_some(text)
    }
}
