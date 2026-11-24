# haest-acp

[haest](https://github.com/haest-werk/haest) as an
[Agent Client Protocol](https://agentclientprotocol.com/) agent: an editor
that speaks ACP (Zed's agent panel, JetBrains, any ACP client) starts haest
and drives whole haest conversations over its stdin and stdout.

The `haest` command serves it as `haest acp`. In Zed's `settings.json`:

```json
"agent_servers": {
  "haest": { "type": "custom", "command": "haest", "args": ["acp", "--profile", "claude"] }
}
```

Any setting flag after `acp` applies to every session the editor opens, over
haest's files for the directory each one names.

## In a host of its own

`AcpAgent` serves one client. The host says how a conversation is built for
the directory the client names, by implementing `Conversations`: the
`RuntimeInputs` (settings, keys, providers, MCP servers) it would build any
conversation from. haest-acp adds the client's MCP servers after the host's,
and its own plan mode, whose listener tells the client the session's mode.

```rust
use async_trait::async_trait;
use haest_acp::{AcpAgent, Conversations, Setup};
use haest_core::{error::BoxError, fs::MemFs};
use haest_runtime::{RuntimeInputs, World};
use haest_settings::Settings;

struct Host;

#[async_trait]
impl Conversations for Host {
    async fn inputs(&self, setup: &Setup) -> Result<RuntimeInputs, BoxError> {
        let settings = Settings::bare(&setup.cwd, "/home/me");
        Ok(RuntimeInputs::new(settings, World::files(MemFs::new())))
    }
}

async fn serve() -> Result<(), haest_acp::AcpError> {
    AcpAgent::new(Host, "/work").serve_stdio().await
}
```

`AcpAgent::serve` takes any byte reader and writer in place of stdin and stdout.

## What maps

| ACP | haest |
|---|---|
| `initialize` | honest capabilities: load, list, resume and close sessions; text, resource links and embedded text in a prompt; MCP over stdio and HTTP |
| `session/new`, `session/load`, `session/resume` | a conversation built from the host's inputs for `cwd`; a load tells the saved history again (messages, tool calls and results, the task list) |
| `session/list`, `session/close` | the saved sessions in the settings' data dir; a conversation ended and saved |
| `session/prompt` | one turn; resource links as their path, embedded text in a `<context>` block; a `/command` runs the conversation's command (`/compact`, a mod's, a skill) |
| `session/cancel` | the turn interrupted; its prompt answers `cancelled` |
| `session/update` | message and thought chunks; tool calls with a kind per tool, the files they touch and an edit's diff; the task list as the plan; the commands; mode and option changes; notices; subagents as child sessions |
| `session/request_permission` | an approval ask on the call it holds up: allow, allow always, deny; withdrawn when the turn is interrupted |
| modes, config options | `default` and `plan` (plan mode); the model (the built-in catalog) and the effort |

## Limitations

- The client's file system (`fs/*`) and terminals (`terminal/*`) are not
  used: haest's tools work in its own workspace (the real disk or its
  sandbox), and an editor's unsaved buffers are not seen.
- No images or audio in a prompt (the runtime has no media input on a human
  turn yet); embedded binary resources are refused too. MCP over SSE is
  refused.
- What the protocol marks unstable is not offered (forking, session status,
  prompt suggestions, MCP over ACP, usage in a prompt's answer, protocol v2),
  nor `session/delete`, a client's system prompt or extra directories.
- No `usage_update` and no compaction updates: a compaction is told as a
  notice.
- The model picker lists the built-in catalog and the current model.

[docs.rs](https://docs.rs/haest-acp/) has the whole table and every
limitation; the repository's
[README](https://github.com/haest-werk/haest/blob/main/README.md) has the rest
of haest.

License: MIT.
