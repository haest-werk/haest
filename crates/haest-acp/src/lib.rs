//! haest as an [Agent Client Protocol](https://agentclientprotocol.com/)
//! agent: an editor that speaks ACP (Zed's agent panel, JetBrains, any ACP
//! client) starts haest and drives whole haest conversations over its stdin
//! and stdout.
//!
//! [`AcpAgent`] serves one client. Each session the client opens is a whole
//! conversation, built by haest-runtime from what the host's
//! [`Conversations`] hands in for the client's directory, and run behind a
//! haest-session session: the client's prompts are its turns, its stream is
//! told the client as `session/update` notifications, and its approval asks
//! are the client's permission requests. The `haest acp` command is this
//! crate over haest's own files (`haest::acp` behind the facade's `acp`
//! feature).
//!
//! ```toml
//! # Zed's settings.json: haest in the agent panel.
//! "agent_servers": { "haest": { "type": "custom", "command": "haest", "args": ["acp"] } }
//! ```
//!
//! # What maps to what
//!
//! | ACP | haest |
//! |---|---|
//! | `initialize` | the capabilities below, honestly: load, list, resume and close sessions; text, resource links and embedded text in a prompt; MCP servers over stdio and HTTP |
//! | `session/new` | a new conversation in `cwd` ([`Conversations::inputs`]), the client's MCP servers after the host's |
//! | `session/load` | the saved session of that id, its history told again (messages, tool calls and results, the task list), then the conversation goes on |
//! | `session/resume` | the same, without the history told again |
//! | `session/list` | the saved sessions in the settings' data dir (`cwd` filters them) |
//! | `session/close` | the conversation ends: what it started stops, its file is saved |
//! | `session/prompt` | one turn; text and resource links as the human's text, an embedded text resource after it in a `<context>` block; a line starting `/` runs the conversation's command first (a mod's, a skill, or `/compact`) |
//! | `session/cancel` | the running turn is interrupted; its prompt answers `cancelled` |
//! | stop reasons | `end_turn`; `max_tokens` for the output limit, a cap or the budget (`max_turn_requests` for the steps cap); `refusal`; `cancelled`; a turn that failed is a JSON-RPC error |
//! | `agent_message_chunk`, `agent_thought_chunk` | the answer's text and reasoning as they stream (the whole answer when nothing streamed) |
//! | `tool_call`, `tool_call_update` | each call as it starts (pending), its input (in progress) and its result (completed or failed); a kind per tool (Read: read, Edit and Write: edit, Grep and Glob: search, Bash: execute, WebFetch: fetch, Agent and the task tools: think); the files it touches; an edit's or a write's diff |
//! | `plan` | the task list, each time it changes |
//! | `available_commands_update` | `/compact`, the mods' commands, the skills a person may call |
//! | `session/request_permission` | an approval ask on the call it holds up: allow, allow always (the same tool and key), deny; withdrawn (`$/cancel_request`) when the turn is interrupted or the ask times out |
//! | modes | `default` and `plan` (haest's plan mode), set by the client or by `/plan`, told with `current_mode_update` |
//! | config options | the model (the built-in catalog) and the effort, told again with `config_option_update` when they change |
//! | `notice` | haest's notices, for a client that takes them (`session.notices`); else warnings and errors as a quoted part of the agent's message |
//! | subagents | for a client that takes them (`subagents`): each subagent a child session, announced on its parent's, its events on its own; else its tool call shows it |
//!
//! # Limitations
//!
//! - The client's file system (`fs/*`) and terminals (`terminal/*`) are not
//!   used. haest's tools work in its own workspace (the real disk or the
//!   sandbox), which needs a whole tree (list, stat, locks, binary files)
//!   where ACP reads and writes one text file at a time; and its shell runs
//!   its own processes. An editor's unsaved buffers are not seen.
//! - No images or audio in a prompt: the runtime has no way in for media on
//!   a human turn yet. haest says so in `initialize`, and refuses either.
//!   Embedded binary resources are refused too.
//! - MCP servers over SSE are refused (haest takes stdio and streamable HTTP).
//! - What the protocol marks unstable is not offered: forking, archiving or
//!   titling a session, session status and cursors, prompt suggestions,
//!   the initial config options of `session/new`, MCP over ACP, a turn's
//!   token usage in its answer, protocol v2.
//! - `session/delete`, a client's system prompt and extra directories are
//!   not offered (haest has no counterparts yet).
//! - No `usage_update` (the context window's fill) and no compaction
//!   updates: a compaction is told as a notice.
//! - The model picker lists the built-in catalog (and the current model);
//!   providers.json's models are set by spec in haest's own settings.
//! - A plan mode the host's inputs carry is replaced by haest-acp's, whose
//!   listener tells the client the mode.
//! - A `/clear` is not offered: a client starts a new session instead.
//!
//! # Examples
//!
//! A host over files of its own, serving a client in the same process over
//! a pipe: [`Conversations`] shows the host's side, `haest acp` serves
//! haest's own files over stdio, and its tests drive it with ACP's client.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod agent;
mod config;
mod error;
mod forward;
mod host;
mod mcp;
mod open;
mod permission;
mod prompt;
mod replay;
mod settings;
mod state;
mod tell;
mod translate;

pub use agent::AcpAgent;
pub use error::AcpError;
pub use host::{Conversations, ConversationsRef, Setup};
