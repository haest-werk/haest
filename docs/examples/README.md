# Examples

Every example runs against a scripted model or fakes: no keys, no network past 127.0.0.1.
`cargo test` runs each one, so an example that stops working fails the suite.

## Programs

From a checkout: `cargo run -p haest --example <name>`, with the features in the last column.

| Example | Shows | Features |
|---|---|---|
| [notebook](../../crates/haest/examples/notebook/) | a host that brings its own parts (a knowledge base whose node runs the agent): its HTTP client, files, tools, clock and turn caps, every plug point asserted | default |
| [auto_compact](../../crates/haest/examples/auto_compact.rs) | compacting as a turn ends past 60% of the window, as a Rust hook and as the mod file [auto-compact.ts](mods/auto-compact.ts), the same conversation for each | default |
| [external_hooks](../../crates/haest/examples/external_hooks.rs) | hooks in another process: a Claude Code hook script refusing `rm -rf`, a reviewer service sending the turn back once | `bridge-shell,bridge-http` |
| [cache_policy](../../crates/haest/examples/cache_policy.rs) | a host's own prompt-cache policy: the system prompt cached an hour, the conversation five minutes | default |
| [threshold](../../crates/haest/examples/threshold.rs) | a host's own thresholds: remind the model, warn the human | default |
| [wind_down](../../crates/haest/examples/wind_down.rs) | wind-down near a cap: no new subagents, the model told once to conclude | default |
| [durable_run](../../crates/haest/examples/durable_run.rs) | a durable run on the memory store: the finished-run shortcut, a kill and a resume | default |
| [wire](../../crates/haest/examples/wire.rs) | both ends of the wire format: frames numbered, kept, read back, caught up | `wire` |
| [session](../../crates/haest/examples/session.rs) | a conversation another program watches and drives: a watcher, input mid-turn, a late peer catching up | `session` |
| [stdio](../../crates/haest/examples/stdio.rs) | a parent process driving a conversation over a pipe pair, the way `haest --stdio` is driven | `transport-stdio` |
| [control_socket](../../crates/haest/examples/control_socket.rs) | a conversation on a Unix socket, found, followed and driven the way `haest ctl` does | `transport-unix` |
| [broker](../../crates/haest/examples/broker.rs) | a conversation that dials out to a broker: `hello` with its token, the ack, the stream | `transport-broker` |
| [issues](../../crates/haest-issues/examples/issues.rs) | issue-tracker tools on an agent planning work on a board of card files (`cargo run -p haest-issues --example issues`) | (haest-issues) |

[plug_in.rs](../../crates/haest/tests/plug_in.rs) is the host that takes haest's own pieces
where the notebook brings its own (a `Provider`, a sandbox shell command, the subagent hub,
steering, skills, deferred tools), from outside the crate, the public API only.

## Files

Each is loaded by a test, so it stays valid.

| File | What |
|---|---|
| [config.toml](config.toml) | a whole `~/.config/haest/config.toml`: model, tools, context, skills, trust, `[cache]`, `[[hooks."tool.before"]]`, `[env]`, profiles |
| [project-config.toml](project-config.toml) | a project's `.haest/config.toml`, read only inside `trusted_dirs` |
| [mcp.json](mcp.json) | a project's `.mcp.json`: a stdio server and an HTTP one |
| [providers.json](providers.json) | more providers, in `<config dir>/providers.json` |
| [env.sh](env.sh) | the same settings as `HAEST_*` exports |
| [hooks/no-rm-rf.sh](hooks/no-rm-rf.sh) | a Claude Code `PreToolUse` script that refuses `rm -rf`, run as it is by Claude Code and by haest |
| [mods/auto-compact.ts](mods/auto-compact.ts) | a mod file that compacts as a turn ends, once the context is past 60% of the window |

Recipes that use them: [howto.md](../howto.md). The first five minutes: [quickstart.md](../quickstart.md).
