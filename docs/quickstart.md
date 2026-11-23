# Quickstart

Two ways in: the `haest` command, or haest embedded in your own Rust program.
Each takes about five minutes. Every command here was run against a real model.

## The command

### 1. Install it and give it a key

```sh
cargo install haest-cli               # puts `haest` on your PATH (macOS, Linux)
export OPENROUTER_API_KEY=...         # anthropic: models take ANTHROPIC_API_KEY, openai: OPENAI_API_KEY
```

From a checkout, `cargo run -p haest-cli --` takes the same arguments as `haest`.

### 2. Ask something

```sh
haest -p "say hi in three words"
haest -m openrouter:anthropic/claude-haiku-5.5 -p "say hi in three words"
```

haest starts bare: a model and nothing else. `-p` answers once and exits; without it you get
the interactive prompt (`/help` lists its commands). The default model is
`openrouter:anthropic/claude-sonnet-5.5`; `haest models` lists the rest.

### 3. Give it tools

```sh
haest --tools read,grep,glob -p "what is this project?"
haest --tools all                     # every built-in tool, in the sandbox
```

The shell and file tools run in a sandbox: a simulated bash over a copy-on-write overlay of the
working dir, where nothing reaches the disk until you `/apply` it. `--workspace real` works on
the disk and the real bash.

### 4. Guard a tool with a Claude Code hook script

Copy [no-rm-rf.sh](examples/hooks/no-rm-rf.sh), a Claude Code `PreToolUse` script, to
`~/.config/haest/hooks/` and bind it in `~/.config/haest/config.toml`:

```toml
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"
```

```sh
haest --tools bash -p "Run exactly this with the Bash tool: rm -rf build"
# x Bash rm -rf build (17 ms)
# The command `rm -rf build` was refused: this environment doesn't allow `rm -rf` ...
```

The script runs as it is under Claude Code and under haest. A hook can also be a URL
(`url = "https://..."`, a POST per event) or speak haest's own JSON on every event
([extending.md](extending.md#hooks-from-a-config-file)).

### 5. Change the agent with a mod file

A mod file is TypeScript (or JavaScript) haest runs in embedded QuickJS. Put this in
`~/.config/haest/mods/banana.ts`:

```ts
export default function (haest: any): void {
  haest.on('system.prompt', () => 'End every answer with the word BANANA.')
}
```

```sh
haest --mods -p "say hi in three words"
# Hello there, friend. BANANA
```

A handler is also handed the session the conversation is on, `{ id, transcriptPath }`, as its
last argument (here its only one, `(session) => ...`); [howto.md](howto.md#know-the-session)
lists where each event's handler gets it. `mods = true` in `config.toml` turns mod files on for
good, and `mod_reload = "auto"` (or `"ask"`) reloads a file you edit before the next model
call, its tools included.
[auto-compact.ts](examples/mods/auto-compact.ts) is a longer one: it compacts the
conversation as a turn ends, once the context is past 60% of the window.

### Next

- [howto.md](howto.md): one recipe per task (hooks, mod files, tools that change, the cache).
- [settings.md](settings.md): every setting, its default, and where it may be set.
- [examples/](examples/): runnable examples and config files, one line each.

## Embedded in Rust

### 1. Depend on it

```toml
[dependencies]
haest = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

### 2. Run one turn

The README's [Embed it](../README.md#embed-it) is a whole program: a real model, haest's
built-in tools, the answer streamed to the terminal. Run it with `OPENROUTER_API_KEY` set.

### 3. Add a hook

Any closure taking the event's payload is a hook. Bind hooks per event, wrap the agent's
tools and hooks with them, and every tool call, model call and turn end passes through. With
`shared` and `tools` from that program, `cwd` its working dir and `ui` your
[`ModUi`](https://docs.rs/haest-mods/latest/haest_mods/trait.ModUi.html) (where an approval is
asked and a notice shown):

```rust
use haest::mods::{Before, Binding, HookSet, ModEnv, ModLoopHooks, ModTools, Mods, ToolBeforePayload};
use haest::{AgentDeps, NoHooks, core::workspace::WorkspaceMode};

let guard = HookSet::new().on_tool_before(
    Binding::new(|call: ToolBeforePayload, _| async move {
        let command = call.call.input.get("command").and_then(|c| c.as_str());
        let rm = command.is_some_and(|c| c.contains("rm -rf"));
        Ok::<_, haest::BoxError>(if rm {
            Before::Deny("rm -rf is not allowed here".into())
        } else {
            Before::Pass
        })
    })
    .with_matcher("Bash"),
);
let mods = Mods::new(ModEnv::new(cwd, WorkspaceMode::Real, ui), [guard]);
let deps = AgentDeps::new(shared, ModTools::new(tools, mods.clone()), "You fix code.")
    .with_hooks(ModLoopHooks::new(NoHooks, mods));
```

Each payload carries the conversation's session (`call.session`, a Claude Code script's
`session_id`) when the env names one: `ModEnv::new(..).with_session(SessionView::new(id))`,
and `mods.env().set_session(..)` when a /clear starts a new session.

Whole programs that compile and run under `cargo test`:
[external_hooks.rs](../crates/haest/examples/external_hooks.rs) (a shell script and a service as
hooks) and [auto_compact.rs](../crates/haest/examples/auto_compact.rs) (a Rust hook and a mod
file, side by side).

### Next

- [extending.md](extending.md): every extension point, what each event may decide, which form to use.
- [examples/](examples/): every runnable example.
- [docs.rs/haest](https://docs.rs/haest/): the API.
