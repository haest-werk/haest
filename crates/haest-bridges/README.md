# haest-bridges

Mods and hooks written outside Rust for
[haest](https://github.com/haest-werk/haest), an agent loop and tool harness
you embed in a Rust program.

A bridge is a `Hook<E>` or a `Mod` from
[haest-mods](https://crates.io/crates/haest-mods) that does three things for
each event: encodes the event's payload, carries it to code that is not the
host's Rust (a script, a process, a server), and decodes that code's answer as
the event's decision. The payloads and decisions are haest-mods' types, and
their JSON (`{ deny }`, `{ input }`, `{ stop }`, `{ steer }`, `{ continue }`,
`{ compact }`) is what every bridge speaks. What a decision does is
haest-mods' too, so a handler means the same whichever bridge carries it.

Each bridge is a feature of this crate, with the dependencies only it needs.
None is on by default; without one the crate holds nothing, and a host that
writes its mods in Rust never links an engine.

- `js`: `.js` / `.ts` mod files in embedded QuickJS (`.ts` is type-stripped
  first, in pure Rust).
  - `JsMod` is one file: its default export receives the `haest` API (hook an
    event, add a tool or a slash command, hide any tool, ask the human). It
    runs on its own thread, bounded by `ModCaps` (memory, a time slice, a load
    timeout); `ModLoadError` says why one did not load.
  - Its handlers are asked one at a time and each answer is decoded on the
    Rust side by haest-mods, as a shell or HTTP hook's is: `{ deny: null }`
    passes, and `{ stop: false }` or a number where text goes is the
    handler's failure, never turned into text.
  - `ModFiles` is a conversation's mod files as one mod: the human's
    (`<config dir>/mods`) and, only in a directory the human trusts, the
    project's (`.haest/mods`), loaded in name order and run by haest-mods'
    rules. A host adds it after the built-ins and before its own mods.
  - With `ModFileOptions::with_reload` (the `mod_reload` setting: `off`,
    `ask`, `auto`), `ModFiles` reads the files again before each model call:
    a new or changed file loads, a deleted one unloads, one that fails to
    load keeps the version before and is reported. `ask` puts each change
    before the human first. The model's next request has the new tools.

- `shell`: `ShellHook`, a hook that is a command, run on the host once per
  event: the event as JSON on its stdin, the decision on its stdout.
- `http`: `HttpHook`, a hook behind a URL: each event is a POST of its JSON,
  and the response's body is the decision.

```rust
use haest_bridges::{ModFileOptions, ModFiles, ModReload};
use haest_core::{MemFs, workspace::WorkspaceMode};
use haest_mods::{ApprovalRequest, Decision, ModEnv, ModOptions, ModUi, load_mods};

struct Terminal;

#[async_trait::async_trait]
impl ModUi for Terminal {
    async fn ask(&self, _: ApprovalRequest, _: &tokio_util::sync::CancellationToken) -> Decision {
        Decision::Deny
    }
    fn info(&self, text: &str) {
        println!("{text}")
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let disk = MemFs::from_files([(
        "/home/me/.config/haest/mods/short.js",
        "export default (haest) => haest.on('system.prompt', () => 'Answer in one line.')",
    )]);
    let env = ModEnv::new("/project", WorkspaceMode::Real, Terminal);
    let opts = ModFileOptions::new("/home/me/.config/haest")
        .with_user_mods(true)
        .with_reload(ModReload::Ask);
    let files = ModFiles::load(opts, disk, env).await;

    let mods = load_mods(ModOptions::new("/project", WorkspaceMode::Real).with_mod(files), Terminal);
    assert_eq!(mods.system_append().await, "Answer in one line.");
}
```

## Hooks in another process: `shell` and `http`

`ShellHook` and `HttpHook` are `Hook<E>`s: bind one per event in
haest-mods' `Hooks`, alone or in a `Binding` with its timeout (60 s by
default), `on_error` and tool matcher, as a Rust closure is bound.

```rust
use haest_bridges::{Claude, HttpHook, ShellHook};
use haest_mods::{Binding, Hooks};

let hooks = Hooks::new()
    // A Claude Code hook script, as it is, on Bash calls only.
    .on_tool_before(
        Binding::new(ShellHook::new("sh .claude/hooks/no-rm-rf.sh").with_dialect(Claude))
            .with_matcher("Bash"),
    )
    // A service that sees every finished turn, in haest's own JSON.
    .on_turn_end(HttpHook::new("https://review.internal/turn-end").with_header("authorization", "Bearer t0k3n"));
```

A command answers with its exit code and stdout:

| Exit | stdout | What it is |
|---|---|---|
| 0 | empty | a pass: the event goes on as it was |
| 0 | the event's decision, in the dialect | that decision |
| 0 | anything else | a failure |
| 2 | (ignored) | the event's blocking answer, stderr its text: `tool.before` refuses the call, `model.before` and `model.after` stop the turn, `turn.end` keeps it going; on any other event, a failure |
| any other | (ignored) | a failure, stderr in its report |

A server answers with its status and body: a 2xx with an empty body passes,
a 2xx with the event's decision is that decision, anything else (another
status, no answer, a body that is no decision, one past 64 MiB) is a failure.
An HTTP hook has no exit 2: a block is an answer.

A failure and a timeout do what the binding's `on_error` says. The defaults
fail closed where it matters: a `tool.before` hook that breaks refuses the
call (Claude Code's default lets it through), and on every other event the
turn goes on and the human sees the failure.

A command runs on the host, never in the agent's sandbox, through
haest-workspace's real exec: `bash -c <command>` in a process group of its
own, killed whole when its binding's time is up or the human interrupts. It
runs in the conversation's directory (`with_cwd`, else a tool event's own),
gets only the variables the host hands in (`with_env`), and the first 64 MiB
of each stream are kept. A POST goes through haest-http's `HttpClient` (the
host's own with `with_client`), with `content-type: application/json` and the
headers set with `with_header`. A hook runs with the host's full trust: load a
project's hooks only from a directory the human trusts, and an HTTP hook only
from the human's own configuration, since it receives every tool input.

A test of an HTTP hook posts to haest-http's `testing::Server` (its
`testing` feature, for `dev-dependencies` only and outside semver).

### From a config file

A CLI binds the same hooks with no Rust. haest-mods' `HookSettings` is the
`hooks` setting haest-config reads: `[[hooks."tool.before"]]` entries, each a
`command` or a `url` with its `dialect`, `matcher`, `timeout`, `on_error` and
`headers`, checked when read (an error names the entry, never a header's
value), or built in Rust with `HookSettings::with_entry`. `HookHost::bind`
binds them, with `shell` or `http` on, in one `Hooks` named `[hooks]`: the
host is the conversation's directory, the environment a command gets (none:
no command binds) and the HTTP client. haest-runtime does this in every
conversation, and names its session on the mods' `ModEnv`;
[docs/extending.md](https://github.com/haest-werk/haest/blob/main/docs/extending.md#hooks-from-a-config-file)
has the keys and the trust rules.

```toml
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"
```

### Dialects

`Haest`, the default, speaks the protocol a mod file's handler does, on every
event: the input is `{ "event": "tool.before", ...payload }` (the payload's own
JSON, the event's name added), the answer is the decision's own JSON
(`{ deny }`, `{ input }`, a tool output, text, `{ stop }`, `{ steer }`,
`{ continue }`, `{ compact }`). When the host names the conversation's session,
the payload has it too: `"session": { "id": "..", "transcriptPath": ".." }`.

Neither dialect holds anything of the conversation: `Haest` and `Claude` are
unit values, and each event's payload brings the session the conversation is on
then. A host names it on the conversation's `ModEnv`, and names the new one when
the human's /clear starts it; hooks bound once follow it:

```rust
let env = ModEnv::new(project, WorkspaceMode::Real, ui).with_session(SessionView::new("s-1"));
let mods = Mods::new(env, [hooks]);
// The human's /clear:
mods.env().set_session(SessionView::new("s-2"));
```

`Claude` speaks Claude Code's hook JSON, so its hook scripts and HTTP hooks run
as they are, on the events Claude Code has:

| haest event | Claude Code event | What its answer does |
|---|---|---|
| `tool.before` | `PreToolUse` | `permissionDecision: "deny"` (or the older `decision: "block"`) refuses the call with its reason; `updatedInput` replaces the whole input; `allow` and `defer` pass; `ask` refuses too, with the hook's reason |
| `tool.after` | `PostToolUse` | `updatedToolOutput` replaces the output (an object as its JSON text); `additionalContext` and a `decision: "block"`'s `reason` are added after the output the model reads |
| `prompt.submit` | `UserPromptSubmit` | `additionalContext`, or plain text, is added after the prompt; `decision: "block"` is a failure |
| `system.prompt` | `SessionStart` | `additionalContext`, or plain text, is added to the system prompt |
| `turn.end` | `Stop` | `decision: "block"` with its `reason`, or `additionalContext`, keeps the turn going with that text |

The input has the fields Claude Code sends that haest has: `hook_event_name`,
`cwd`, `session_id` and `transcript_path` when the host names the session (as
of each event: after a /clear, the new one), and per event `tool_name`,
`tool_input`, `tool_use_id`, `tool_response` (the output's text), `prompt`,
`stop_hook_active` and `last_assistant_message`. A command also gets
`CLAUDE_PROJECT_DIR`. Which events Claude Code has is haest-mods'
`ModEvent::claude_code`: the config check, the names a hook is handed and the
events a `Claude` hook compiles on all come from it.

Where Claude Code can do more than haest's event, haest fails closed or
ignores it:

- `ask` cannot reach the human from a hook: it refuses the call.
- `prompt.submit` cannot refuse a prompt: `decision: "block"` and exit 2 are
  failures, reported.
- Exit 2 on `PostToolUse` is a failure, where Claude Code shows stderr to the
  model.
- `continue: false`, `stopReason`, `systemMessage`, `suppressOutput` and
  `PreToolUse`'s `additionalContext` have no haest decision: ignored.
- Matchers are haest's (an exact tool name or a `prefix*`), never Claude
  Code's regular expressions.
- `model.before` and `model.after` have no Claude Code event: a `Claude` hook
  bound to one does not compile.
- `transcript_path` names the host's own transcript, which is not Claude
  Code's format; a script that parses it reads it wrong.
- A command gets only the variables the host hands in, where a Claude Code
  hook inherits Claude Code's whole environment.

Through the facade this crate is `haest::bridges` (features `bridge-js`, on by
default, `bridge-shell` and `bridge-http`, off). The docs on [docs.rs](https://docs.rs/haest-bridges/) have the
details; the repository's
[README](https://github.com/haest-werk/haest/blob/main/README.md) has the rest
of haest.

License: MIT.
