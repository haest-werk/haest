# Hooks: what each event sends, and what it may answer

<!-- Written from the dialects (crates/haest-bridges/src/dialect/reference.rs):
     every input below is what haest sends, every reading what haest reads.
     Edit the code, then rewrite this file:
     HAEST_BLESS=1 cargo test -p haest-bridges --features shell hooks_reference -->

A hook in another process is a command or a URL, bound to one event. haest
sends it the event as JSON (a command on its stdin, a URL as a POST's body)
and reads its answer as the event's decision. This page has every event's
input and the answers it takes, in both dialects, and every option an entry
has.

A test writes this page from the code that encodes the input and decodes the
answer, and fails when the two disagree: each example input is what haest
sends, and each "haest reads" cell is what haest makes of that answer.

Bind one with a [`hooks`](settings.md#hooks) entry in a config file (below),
or in Rust with `ShellHook` or `HttpHook` in a `HookSet`
([extending.md](extending.md#hooks-in-another-process)). A Rust hook and a
mod file's handler get the same payloads and give the same decisions:
[extending.md](extending.md#what-each-event-may-decide).

| Event | Fires | In Claude Code | Exit 2 | `on_error` | `matcher` |
|---|---|---|---|---|---|
| [`tool.before`](#toolbefore) | before every tool call, a subagent's too | `PreToolUse` | `{"deny":<stderr>}` | `block` | yes |
| [`tool.after`](#toolafter) | after a tool call ran, before the model reads its output | `PostToolUse` | a failure | `continue` | yes |
| [`prompt.submit`](#promptsubmit) | when the human sends a message, before the model sees it | `UserPromptSubmit` | a failure | `continue` | no |
| [`system.prompt`](#systemprompt) | before every model call, a subagent's too | `SessionStart` | a failure | `continue` | no |
| [`model.before`](#modelbefore) | before every model call, after the host's own `before_model_call` | none | `{"stop":<stderr>}` | `continue` | no |
| [`model.after`](#modelafter) | when an answer lands, before its tool calls run | none | `{"stop":<stderr>}` | `continue` | no |
| [`turn.end`](#turnend) | when a turn is about to end on its own | `Stop` | `{"continue":<stderr>}` | `continue` | no |

Exit 2 is a command's blocking answer: the decision it makes, its stderr the text (the model reads 30000 characters of it at most, cut in the middle past that, as a tool's output is). `on_error` is what a failure does unless the entry says. `matcher`: whether an entry may name the tools it is for.

## A command and a URL

| | `command` (`ShellHook`) | `url` (`HttpHook`) |
|---|---|---|
| runs | `bash -c <command>` on the host, never in the sandbox, in the conversation's directory | a POST through the runtime's `HttpClient`, the one the model calls use |
| the input | on stdin | the body, `content-type: application/json` |
| it gets | the environment every process haest starts gets ([`env_passthrough`](settings.md#env_passthrough), [`env`](settings.md#env)), plus its dialect's variables | the entry's `headers` |
| the answer | stdout, on exit 0 | the body of a 2xx response |
| an empty answer | passes | passes |
| a blocking answer | exit 2, stderr its text (the Exit 2 column above) | the decision itself: a URL has no exit 2 |
| a failure | any other exit, or stdout that is no decision | another status, no answer, or a body that is no decision |
| at the timeout or an interrupt | its whole process group is killed | the request is dropped |
| how much it may write | the first 64 MiB of stdout and of stderr are kept | a body past 64 MiB is a failure |

A failure, a timeout and a command that cannot start all do what the entry's
`on_error` says. `continue` reports it to the human and goes on as if the hook
had passed: the next hook is asked. `block` ends the event there: `tool.before`
refuses the call (the model reads why), `model.before` and `model.after` stop
the turn, and on any other event what the hooks did is dropped and the human
sees the failure. The defaults fail closed where it matters: a broken
`tool.before` hook refuses the call, where Claude Code's default lets it
through.

## Options

One `[[hooks."<event>"]]` entry per hook, under the event's name, quoted for
its dot (`[[hooks.tool.before]]` is a table `tool` to TOML, which haest refuses
with a hint):

```toml
# A Claude Code PreToolUse script, as it is, on Bash calls.
[[hooks."tool.before"]]
command = "sh ~/.config/haest/hooks/no-rm-rf.sh"
dialect = "claude"
matcher = "Bash"
timeout = 10
on_error = "block"

# A reviewer that may keep a turn going, in haest's own JSON.
[[hooks."turn.end"]]
url = "https://review.example/haest/turn-end"
headers = { authorization = "Bearer ${REVIEW_TOKEN}" }
on_error = "continue"
```

| Key | Takes | Default |
|---|---|---|
| `command` | a command, run as `bash -c <command>` on the host per event, in the conversation's directory: a `ShellHook`. Give it as an absolute path or under `~/` in your own file: a relative one runs whatever the working dir has at that path |  |
| `url` | an `http://` or `https://` URL, POSTed per event: an `HttpHook` |  |
| `dialect` | `haest` (every event) or `claude` (Claude Code's hook JSON, on the events Claude Code has) | `haest` |
| `matcher` | on `tool.before` and `tool.after` only: a tool's name (`Bash`) or a prefix ending in `*` (`mcp__github__*`). Claude Code's `Edit\|Write` and `Bash.*` are regular expressions, which haest does not read, and `Edit,Write` is a list: write one entry per tool | every tool |
| `timeout` | seconds (`10`, `2.5`); past them the hook failed. Claude Code's key, as it is: every other time setting that takes a number takes minutes, and `[timeouts]` takes text with its unit | `[timeouts] hook`, 60 s unless set |
| `on_error` | what a failure or a timeout does: `block` or `continue` | `block` on `tool.before`, else `continue` |
| `headers` | a `url`'s headers, `{ name = "value" }`; `${NAME}` and `${NAME:-default}` in a value are filled in from haest's environment | none |

An entry takes `command` or `url`, never both. Loading refuses an unknown event
or key, an entry with neither or both, `dialect = "claude"` on an event Claude
Code does not have, a `matcher` on an event that is not about a tool, and
`headers` on a command. The error names the file and the entry as that file
numbers it
(``~/.config/haest/config.toml: hooks: [[hooks."turn.end"]] #2: unknown key `comand` ``),
and never a header's value. A Rust host builds the same entries without a file:
haest-mods' `HookSettings::with_entry`, with a `HookEntry::shell` or a
`HookEntry::http`, refused what a file is refused.

### Where entries come from

- Every layer's entries bind after the ones before it, in the order written:
  your file, a trusted project's file, the profile, `HAEST_HOOKS`, then each
  `--hooks`. A variable or a flag holds the same as TOML:
  `--hooks '{ "turn.end" = [{ command = "sh check.sh" }] }'`.
- A project's `.haest/config.toml` is read only inside your
  [`trusted_dirs`](settings.md#trusted_dirs), so an untrusted repo's hooks
  never load. A trusted one may bind a `command` (it already runs its own code
  through its mods and its `.mcp.json`), never a `url`: a POST sends the
  conversation's events where the repo says. A `url` comes from your own file,
  a variable or a flag; a project's file, or a profile it defines, that holds
  one fails to load.
- Off by default: no entry, no hook.

### How they run

- The entries are one mod, `[hooks]` in reports (`Mod [hooks] failed in
  tool.before: hook 2 on tool.before failed: ..`, where `hook 2` is the
  event's second bound entry across the layers), after the built-in mods and
  your mod files, before a host's own mods.
- A command runs with the environment every process haest starts gets: `~` in
  a command needs `HOME`, which `env_passthrough = "core"`, the default,
  passes. A host whose
  world starts no process (haest-runtime's `World::files`) binds no command:
  it warns, and binds the rest.
- haest-runtime names the conversation's session on its mods' env, and the new
  one at each /clear: a hook gets the session's id and its file as of that
  event, `session` in `haest`, `session_id` and `transcript_path` in `claude`.
- A subagent's tool calls pass through the hooks, and its system prompt takes
  what `system.prompt` adds. `prompt.submit`, `model.before`, `model.after` and
  `turn.end` are the main conversation's: a subagent's turn is not the
  human's, and a check that keeps the main turn going would hold every
  subagent's to it too.
- `haest settings` shows a header by its name only.
- A hook runs with the host's full trust: it receives every tool input it is
  bound to, and a command runs on the host whatever the workspace.

## The two dialects

| Dialect | The input | The answer | Events |
|---|---|---|---|
| `haest`, the default | `{ "event": "<name>", ...payload }`: the payload a mod file's handler gets, with the event's name | the decision's own JSON, what a mod file's handler returns; a string answer is a JSON string, quoted | every one |
| `claude` | Claude Code's hook JSON (`hook_event_name`, `session_id`, `tool_name`, ..) | Claude Code's (`hookSpecificOutput`, `decision: "block"`, plain text) | the five Claude Code has |

In `haest`, an answer is read by the one decoder every form shares: a key whose
value is `null` counts as absent, and a value of the wrong type (`{ "stop":
false }`, a number from `prompt.submit`) is a failure, never turned into text.

In `claude`, a Claude Code hook script runs as it is:

- Output that starts with `{` must be a JSON object. Other text is plain text,
  which `prompt.submit` and `system.prompt` take and the other events ignore,
  as Claude Code does.
- `continue: false`, `stopReason`, `systemMessage` and `suppressOutput` have no
  haest decision and are ignored. Where Claude Code can do more than haest's
  event (an `ask`, refusing a prompt) haest refuses the call or fails.
- `transcript_path` names haest's own session file, which is not Claude Code's
  format: a script that parses one reads the other wrong.
- Matchers are haest's (a name, or a prefix ending in `*`), never Claude
  Code's regular expressions.
- A hook from Claude Code's `.claude/settings.json` moves over one entry at a
  time: the event by its haest name (the table at the top), its `command` and
  `timeout`, its `matcher` (one entry per tool), and `dialect = "claude"`.

## `tool.before`

Fires before every tool call, a subagent's too. Rewrites chain: each hook sees the input the one before left. The first deny wins, and no hook after it is asked.

### `tool.before` in `haest`

```json
{
  "call": {
    "input": {
      "command": "rm -rf build",
      "description": "Clean the build"
    },
    "name": "Bash",
    "readOnly": false
  },
  "ctx": {
    "cwd": "/home/me/project",
    "toolCallId": "toolu_01"
  },
  "event": "tool.before",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  }
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `call.name` | the tool, as the model called it |
| `call.input` | the input the tool will run with: the model's, or what the hooks before this one left |
| `call.readOnly` | the tool declares it changes nothing |
| `ctx.cwd` | the conversation's working directory |
| `ctx.toolCallId` | the call's id; `null` when the loop gave it none |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The call runs as it is. |
| `{"deny":"not on this machine"}` | `{"deny":"not on this machine"}` | The call is refused and does not run. The model reads `Refused: not on this machine` as its result. |
| `{"input":{"command":"rm -rf ./build","description":"Clean the build"}}` | `{"input":{"command":"rm -rf ./build","description":"Clean the build"}}` | The call runs with this input, and the hooks after this one see it. It replaces the whole input: send every field you keep. |
| `{"deny":null}` | pass | `null` counts as absent: the call runs. |
| `{"deny":false}` | a failure: tool.before: the hook's answer is not a decision it takes | A failure: `on_error`, by default `block`, refuses the call. |
| exit 2, stderr `rm -rf is not allowed here` | `{"deny":"rm -rf is not allowed here"}` | A command only: the call is refused, stderr the reason. |

### `tool.before` in `claude`

```json
{
  "cwd": "/home/me/project",
  "hook_event_name": "PreToolUse",
  "session_id": "s-1",
  "tool_input": {
    "command": "rm -rf build",
    "description": "Clean the build"
  },
  "tool_name": "Bash",
  "tool_use_id": "toolu_01",
  "transcript_path": "/home/me/.local/share/haest/sessions/s-1.jsonl"
}
```

A command also gets `CLAUDE_PROJECT_DIR=/home/me/project`.

| Field | What |
|---|---|
| `session_id` | the session's id; left out when the host names none |
| `transcript_path` | haest's session file, not Claude Code's format; left out when the host keeps none |
| `cwd` | the conversation's directory |
| `hook_event_name` | Claude Code's name for the event |
| `tool_name` | the tool, as the model called it |
| `tool_input` | the input the tool will run with, as the hooks before this one left it |
| `tool_use_id` | the call's id; left out when the loop gave it none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The call runs as it is. |
| `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not on this machine"}}` | `{"deny":"not on this machine"}` | Refused, with the reason. |
| `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"rm -rf ./build"}}}` | `{"input":{"command":"rm -rf ./build"}}` | The call runs with this input: the whole input, as in Claude Code. |
| `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}` | `{"deny":"a hook asked for the human's approval, which a hook cannot get"}` | Refused: a hook cannot ask the human for approval. |
| `{"decision":"block","reason":"not on this machine"}` | `{"deny":"not on this machine"}` | Claude Code's older form: refused. |
| `{"continue":false,"stopReason":"stop everything"}` | pass | No haest decision: ignored, the call runs. |
| exit 2, stderr `rm -rf is not allowed here` | `{"deny":"rm -rf is not allowed here"}` | Refused, stderr the reason. |

## `tool.after`

Fires after a tool call ran, before the model reads its output. Each hook sees the output the one before left. What the call spent stays the tool's.

An output carries more fields when the tool set them: `title`, `result`, `media`, `costUsd`, `childCostUsd`.

### `tool.after` in `haest`

```json
{
  "call": {
    "input": {
      "command": "cargo test"
    },
    "name": "Bash",
    "readOnly": false
  },
  "ctx": {
    "cwd": "/home/me/project",
    "toolCallId": "toolu_01"
  },
  "event": "tool.after",
  "output": {
    "content": "test result: ok. 48 passed; 0 failed",
    "isError": false,
    "sized": false
  },
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  }
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `call.name` | the tool, as the model called it |
| `call.input` | the input the tool will run with: the model's, or what the hooks before this one left |
| `call.readOnly` | the tool declares it changes nothing |
| `ctx.cwd` | the conversation's working directory |
| `ctx.toolCallId` | the call's id; `null` when the loop gave it none |
| `output.content` | what the model reads |
| `output.isError` | the call failed; `content` says why |
| `output.sized` | the tool fit `content` to its own budget, so haest does not spill it to a file |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The model reads the output as it is. |
| `{"content":"48 passed (the log is in test.log)","isError":false}` | `{"content":"48 passed (the log is in test.log)","isError":false,"sized":false}` | This output replaces the call's: the model reads it, and the hooks after this one see it. |
| exit 2, stderr `too late` | a failure: tool.after has no blocking answer, so exit 2 is a failure: too late | `tool.after` has no blocking answer: a failure, and by default (`continue`) the output stays. |

### `tool.after` in `claude`

```json
{
  "cwd": "/home/me/project",
  "hook_event_name": "PostToolUse",
  "session_id": "s-1",
  "tool_input": {
    "command": "cargo test"
  },
  "tool_name": "Bash",
  "tool_response": "test result: ok. 48 passed; 0 failed",
  "tool_use_id": "toolu_01",
  "transcript_path": "/home/me/.local/share/haest/sessions/s-1.jsonl"
}
```

A command also gets `CLAUDE_PROJECT_DIR=/home/me/project`.

| Field | What |
|---|---|
| `session_id` | the session's id; left out when the host names none |
| `transcript_path` | haest's session file, not Claude Code's format; left out when the host keeps none |
| `cwd` | the conversation's directory |
| `hook_event_name` | Claude Code's name for the event |
| `tool_name` | the tool, as the model called it |
| `tool_input` | the input the tool will run with, as the hooks before this one left it |
| `tool_use_id` | the call's id; left out when the loop gave it none |
| `tool_response` | the output's text, as Claude Code sends a failed call's |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The model reads the output as it is. |
| `{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"Coverage fell to 71%."}}` | `{"content":"test result: ok. 48 passed; 0 failed\n\nCoverage fell to 71%.","isError":false,"sized":false}` | Added after the output the model reads. |
| `{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"48 passed"}}` | `{"content":"48 passed","isError":false,"sized":false}` | A new output: the model reads this text. An object is sent as its JSON. |
| `{"decision":"block","reason":"The tests reached the network."}` | `{"content":"test result: ok. 48 passed; 0 failed\n\nThe tests reached the network.","isError":false,"sized":false}` | The call already ran: the reason is added after the output. |
| exit 2, stderr `too late` | a failure: tool.after has no blocking answer, so exit 2 is a failure: too late | A failure, and by default the output stays. |

## `prompt.submit`

Fires when the human sends a message, before the model sees it. Rewrites chain: each hook sees the text the one before left.

### `prompt.submit` in `haest`

```json
{
  "event": "prompt.submit",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  },
  "text": "fix the failing test"
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `text` | the message, as the hooks before this one left it |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The model gets the message as it is. |
| `"fix the failing test\n\nThe CI log is in ci.log."` | `"fix the failing test\n\nThe CI log is in ci.log."` | The model gets this text instead. |
| `fix the failing test` | a failure: prompt.submit: the hook's answer is not a decision it takes | A failure: a string answer is a JSON string, quoted. By default (`continue`) the message goes as it was. |
| exit 2, stderr `no` | a failure: prompt.submit has no blocking answer, so exit 2 is a failure: no | `prompt.submit` has no blocking answer: a failure. |

### `prompt.submit` in `claude`

```json
{
  "cwd": "/home/me/project",
  "hook_event_name": "UserPromptSubmit",
  "prompt": "fix the failing test",
  "session_id": "s-1",
  "transcript_path": "/home/me/.local/share/haest/sessions/s-1.jsonl"
}
```

A command also gets `CLAUDE_PROJECT_DIR=/home/me/project`.

| Field | What |
|---|---|
| `session_id` | the session's id; left out when the host names none |
| `transcript_path` | haest's session file, not Claude Code's format; left out when the host keeps none |
| `cwd` | the conversation's directory |
| `hook_event_name` | Claude Code's name for the event |
| `prompt` | the message, as the hooks before this one left it |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The model gets the message as it is. |
| `The CI log is in ci.log.` | `"fix the failing test\n\nThe CI log is in ci.log."` | Plain text is added after the message. |
| `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"The CI log is in ci.log."}}` | `"fix the failing test\n\nThe CI log is in ci.log."` | The same, as JSON. |
| `{"decision":"block","reason":"no secrets in prompts"}` | a failure: prompt.submit: the hook refused the prompt, which haest's prompt.submit cannot do: no secrets in prompts | haest cannot refuse a prompt: a failure, and by default the message goes as it was. |
| exit 2, stderr `no` | a failure: prompt.submit has no blocking answer, so exit 2 is a failure: no | A failure. |

## `system.prompt`

Fires before every model call, a subagent's too. The texts are joined, a blank line between.

It runs before every model call, so its text is in every request: an answer that changes from one call to the next changes the system prompt, and the prompt cache is written again from there. Claude Code's `SessionStart` runs once per session; under haest a `SessionStart` script runs before every call.

### `system.prompt` in `haest`

```json
{
  "event": "system.prompt",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  }
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | Nothing is added. |
| `` "The repo is on main. Run `make check` before you commit." `` | `` "The repo is on main. Run `make check` before you commit." `` | Appended to the system prompt. |
| `false` | pass | Nothing is added: JavaScript's `cond && 'text'` answers this. |
| exit 2, stderr `no` | a failure: system.prompt has no blocking answer, so exit 2 is a failure: no | `system.prompt` has no blocking answer: a failure. |

### `system.prompt` in `claude`

```json
{
  "cwd": "/home/me/project",
  "hook_event_name": "SessionStart",
  "session_id": "s-1",
  "transcript_path": "/home/me/.local/share/haest/sessions/s-1.jsonl"
}
```

A command also gets `CLAUDE_PROJECT_DIR=/home/me/project`.

| Field | What |
|---|---|
| `session_id` | the session's id; left out when the host names none |
| `transcript_path` | haest's session file, not Claude Code's format; left out when the host keeps none |
| `cwd` | the conversation's directory |
| `hook_event_name` | Claude Code's name for the event |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | Nothing is added. |
| `The repo is on main.` | `"The repo is on main."` | Plain text is appended to the system prompt. |
| `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"The repo is on main."}}` | `"The repo is on main."` | The same, as JSON. |
| exit 2, stderr `no` | a failure: system.prompt has no blocking answer, so exit 2 is a failure: no | A failure. |

## `model.before`

Fires before every model call, after the host's own `before_model_call`. The first stop wins; every steer is kept; the first compact counts.

### `model.before` in `haest`

```json
{
  "contextTokens": 48210,
  "contextWindow": 1000000,
  "effort": "high",
  "event": "model.before",
  "historyLength": 12,
  "lastMessage": {
    "content": "fix the failing test",
    "role": "user"
  },
  "modelSpec": "openrouter:anthropic/claude-sonnet-5.5",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  }
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `modelSpec` | the model in use, `provider:model` |
| `effort` | the effort asked for; `null` when none is |
| `historyLength` | how many messages the history holds |
| `contextTokens` | what the last model call put in the context window, in tokens; 0 before any. The number to compact on |
| `contextWindow` | the model's context window in tokens, from the host's turn caps or the catalog; `null` when neither knows it |
| `lastMessage` | the last message in the history, as the session file writes it less media bytes, thinking signatures and provider state; `null` in an empty history |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The call is made. |
| `{"compact":{"instructions":"keep every file path"}}` | `{"compact":{"instructions":"keep every file path"}}` | haest compacts the conversation before the call. The instructions go after haest's own summary prompt; `true` asks for that alone. |
| `{"steer":"Run the tests before you answer."}` | `{"steer":"Run the tests before you answer."}` | Queued as a user message: it lands after this call's tool results, or after an answer with none, which then does not end the turn. |
| `{"stop":"over this turn's budget"}` | `{"stop":"over this turn's budget"}` | The turn ends before the call, the reason its error. |
| `{"stop":false}` | a failure: model.before: the hook's answer is not a decision it takes | A failure: `stop` is text. By default (`continue`) the call is made. |
| exit 2, stderr `over this turn's budget` | `{"stop":"over this turn's budget"}` | A command only: the turn ends before the call. |

## `model.after`

Fires when an answer lands, before its tool calls run. The first stop wins, and the answer's calls do not run; every steer is kept.

### `model.after` in `haest`

```json
{
  "answer": {
    "content": [
      {
        "id": "toolu_02",
        "input": {
          "command": "cargo test"
        },
        "name": "Bash",
        "type": "tool_call"
      }
    ],
    "model": "anthropic/claude-sonnet-5.5",
    "provider": "openrouter",
    "stopReason": "tool_use",
    "usage": {
      "cacheRead": 0,
      "cacheWrite": 0,
      "input": 10,
      "output": 5,
      "reasoning": 0
    }
  },
  "contextTokens": 48210,
  "contextWindow": 1000000,
  "effort": "high",
  "event": "model.after",
  "historyLength": 12,
  "modelSpec": "openrouter:anthropic/claude-sonnet-5.5",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  }
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `modelSpec` | the model in use, `provider:model` |
| `effort` | the effort asked for; `null` when none is |
| `historyLength` | how many messages the history holds |
| `contextTokens` | what the last model call put in the context window, in tokens; 0 before any. The number to compact on |
| `contextWindow` | the model's context window in tokens, from the host's turn caps or the catalog; `null` when neither knows it |
| `answer` | the answer, as the session file writes it less media bytes, thinking signatures and provider state |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The answer's tool calls run. |
| `{"steer":"Cite the file you read."}` | `{"steer":"Cite the file you read."}` | Queued as a user message: it lands after this answer's tool results. |
| `{"stop":"the answer calls a tool it must not"}` | `{"stop":"the answer calls a tool it must not"}` | The turn ends; the answer's calls do not run. |
| exit 2, stderr `the answer calls a tool it must not` | `{"stop":"the answer calls a tool it must not"}` | A command only: the turn ends. |

## `turn.end`

Fires when a turn is about to end on its own. The first continue wins; the first compact counts.

A hook that keeps the turn going checks `continued` (`stop_hook_active` in `claude`): without that, a turn it holds runs on to its caps.

### `turn.end` in `haest`

```json
{
  "answer": {
    "content": [
      {
        "text": "All 48 tests pass.",
        "type": "text"
      }
    ],
    "model": "anthropic/claude-sonnet-5.5",
    "provider": "openrouter",
    "stopReason": "end_turn",
    "usage": {
      "cacheRead": 0,
      "cacheWrite": 0,
      "input": 10,
      "output": 5,
      "reasoning": 0
    }
  },
  "contextTokens": 48210,
  "contextWindow": 1000000,
  "continued": false,
  "effort": "high",
  "error": null,
  "event": "turn.end",
  "historyLength": 12,
  "modelSpec": "openrouter:anthropic/claude-sonnet-5.5",
  "session": {
    "id": "s-1",
    "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl"
  },
  "stopReason": "end_turn"
}
```

| Field | What |
|---|---|
| `event` | the event's name |
| `modelSpec` | the model in use, `provider:model` |
| `effort` | the effort asked for; `null` when none is |
| `historyLength` | how many messages the history holds |
| `contextTokens` | what the last model call put in the context window, in tokens; 0 before any. The number to compact on |
| `contextWindow` | the model's context window in tokens, from the host's turn caps or the catalog; `null` when neither knows it |
| `stopReason` | why the turn ends, as its word |
| `error` | what went wrong; `null` for a turn that ended normally |
| `answer` | the answer that ends the turn, as `model.after` has it; `null` when the history does not end in one |
| `continued` | a `turn.end` decision already kept this turn going |
| `session.id` | the session's id, as the host names it. `session` is left out when the host names none |
| `session.transcriptPath` | the host's transcript of the session; left out when it keeps none |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The turn ends. |
| `{"continue":"Run the tests and fix what fails."}` | `{"continue":"Run the tests and fix what fails."}` | The turn goes on: the text goes to the model as a user message. |
| `{"compact":true}` | `{"compact":true}` | haest compacts as the turn ends, so the next turn starts small. |
| `{"continue":1}` | a failure: turn.end: the hook's answer is not a decision it takes | A failure: `continue` is text. By default the turn ends. |
| exit 2, stderr `Run the tests and fix what fails.` | `{"continue":"Run the tests and fix what fails."}` | A command only: the turn goes on, stderr the text. |

### `turn.end` in `claude`

```json
{
  "cwd": "/home/me/project",
  "hook_event_name": "Stop",
  "last_assistant_message": "All 48 tests pass.",
  "session_id": "s-1",
  "stop_hook_active": false,
  "transcript_path": "/home/me/.local/share/haest/sessions/s-1.jsonl"
}
```

A command also gets `CLAUDE_PROJECT_DIR=/home/me/project`.

| Field | What |
|---|---|
| `session_id` | the session's id; left out when the host names none |
| `transcript_path` | haest's session file, not Claude Code's format; left out when the host keeps none |
| `cwd` | the conversation's directory |
| `hook_event_name` | Claude Code's name for the event |
| `stop_hook_active` | a `turn.end` decision already kept this turn going |
| `last_assistant_message` | the answer's text; left out when the history does not end in an answer |

| The hook answers | haest reads | What happens |
|---|---|---|
| nothing | pass | The turn ends. |
| `{"decision":"block","reason":"Run the tests and fix what fails."}` | `{"continue":"Run the tests and fix what fails."}` | The turn goes on with the reason as a user message. |
| `{"decision":"block"}` | a failure: turn.end: `decision: "block"` needs a `reason`: the text the turn goes on with | A failure: the turn would go on with no text. By default the turn ends. |
| `{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"Run the tests."}}` | `{"continue":"Run the tests."}` | The turn goes on with the text. |
| exit 2, stderr `Run the tests and fix what fails.` | `{"continue":"Run the tests and fix what fails."}` | The turn goes on, stderr the text. |
