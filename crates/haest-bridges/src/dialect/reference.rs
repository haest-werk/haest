//! `docs/hooks.md`, written from the dialects: for every event, the input a
//! hook in another process gets in each dialect (what [`outgoing`] writes)
//! and what each answer becomes (what the dialect's `decide` reads). Each
//! field table must name exactly the fields the input has, and the config
//! example must load. The doc on disk must be what this writes:
//! `HAEST_BLESS=1 cargo test -p haest-bridges --features shell hooks_reference`
//! rewrites it.

use std::{collections::BTreeSet, fmt::Write, path::Path};

use haest_core::{
    AssistantMessage, Message,
    testing::{text_answer, tool_answer},
    tool::TRUNCATE_CHARS,
};
use haest_mods::{
    DEFAULT_HOOK_TIMEOUT, Event, HookDialect, HookEntry, HookSettings, ModEvent, ModelAfterPayload,
    ModelBeforePayload, PromptSubmitPayload, SystemPromptPayload, ToolAfterPayload,
    ToolBeforePayload, TurnEndPayload,
};
use serde_json::{Value, json};

use super::{Claude, Dialect, Haest, HookFailure, Reply, outgoing};

const DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/hooks.md");

/// The conversation's directory in every example.
const DIR: &str = "/home/me/project";

/// What exit 2's stderr stands for in the event table.
const STDERR: &str = "STDERR";

const INTRO: &str = "# Hooks: what each event sends, and what it may answer

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
sends, and each \"haest reads\" cell is what haest makes of that answer.

Bind one with a [`hooks`](settings.md#hooks) entry in a config file (below),
or in Rust with `ShellHook` or `HttpHook` in a `HookSet`
([extending.md](extending.md#hooks-in-another-process)). A Rust hook and a
mod file's handler get the same payloads and give the same decisions:
[extending.md](extending.md#what-each-event-may-decide).
";

const TRANSPORTS: &str = "
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
";

const OPTIONS_INTRO: &str = "
## Options

One `[[hooks.\"<event>\"]]` entry per hook, under the event's name, quoted for
its dot (`[[hooks.tool.before]]` is a table `tool` to TOML, which haest refuses
with a hint):
";

/// Every key an entry takes. The test loads [`EXAMPLE`], which uses each.
const EXAMPLE: &str = r#"# A Claude Code PreToolUse script, as it is, on Bash calls.
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
"#;

/// `(key, takes, default)`; `{timeout}` is filled in.
const KEYS: &[(&str, &str, &str)] = &[
    (
        "command",
        "a command, run as `bash -c <command>` on the host per event, in the conversation's directory: a `ShellHook`. Give it as an absolute path or under `~/` in your own file: a relative one runs whatever the working dir has at that path",
        "",
    ),
    (
        "url",
        "an `http://` or `https://` URL, POSTed per event: an `HttpHook`",
        "",
    ),
    (
        "dialect",
        "`haest` (every event) or `claude` (Claude Code's hook JSON, on the events Claude Code has)",
        "`haest`",
    ),
    (
        "matcher",
        "on `tool.before` and `tool.after` only: a tool's name (`Bash`) or a prefix ending in `*` (`mcp__github__*`). Claude Code's `Edit\\|Write` and `Bash.*` are regular expressions, which haest does not read, and `Edit,Write` is a list: write one entry per tool",
        "every tool",
    ),
    (
        "timeout",
        "seconds (`10`, `2.5`); past them the hook failed. Claude Code's key, as it is: every other time setting that takes a number takes minutes, and `[timeouts]` takes text with its unit",
        "`[timeouts] hook`, {timeout} s unless set",
    ),
    (
        "on_error",
        "what a failure or a timeout does: `block` or `continue`",
        "`block` on `tool.before`, else `continue`",
    ),
    (
        "headers",
        "a `url`'s headers, `{ name = \"value\" }`; `${NAME}` and `${NAME:-default}` in a value are filled in from haest's environment",
        "none",
    ),
];

const OPTIONS_AFTER: &str = "
An entry takes `command` or `url`, never both. Loading refuses an unknown event
or key, an entry with neither or both, `dialect = \"claude\"` on an event Claude
Code does not have, a `matcher` on an event that is not about a tool, and
`headers` on a command. The error names the file and the entry as that file
numbers it
(``~/.config/haest/config.toml: hooks: [[hooks.\"turn.end\"]] #2: unknown key `comand` ``),
and never a header's value. A Rust host builds the same entries without a file:
haest-mods' `HookSettings::with_entry`, with a `HookEntry::shell` or a
`HookEntry::http`, refused what a file is refused.

### Where entries come from

- Every layer's entries bind after the ones before it, in the order written:
  your file, a trusted project's file, the profile, `HAEST_HOOKS`, then each
  `--hooks`. A variable or a flag holds the same as TOML:
  `--hooks '{ \"turn.end\" = [{ command = \"sh check.sh\" }] }'`.
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
  a command needs `HOME`, which `env_passthrough = \"core\"`, the default,
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
";

const DIALECTS: &str = "
## The two dialects

| Dialect | The input | The answer | Events |
|---|---|---|---|
| `haest`, the default | `{ \"event\": \"<name>\", ...payload }`: the payload a mod file's handler gets, with the event's name | the decision's own JSON, what a mod file's handler returns; a string answer is a JSON string, quoted | every one |
| `claude` | Claude Code's hook JSON (`hook_event_name`, `session_id`, `tool_name`, ..) | Claude Code's (`hookSpecificOutput`, `decision: \"block\"`, plain text) | the five Claude Code has |

In `haest`, an answer is read by the one decoder every form shares: a key whose
value is `null` counts as absent, and a value of the wrong type (`{ \"stop\":
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
  `timeout`, its `matcher` (one entry per tool), and `dialect = \"claude\"`.
";

/// How a dialect hands over `E` and reads an answer to it, as plain
/// functions: one section is written for the events `Claude` speaks and the
/// ones it does not.
struct Speaks<E: Event> {
    name: &'static str,
    input: Box<dyn Fn(&E) -> Value>,
    env: Vec<(String, String)>,
    decide: Box<dyn Fn(E, Reply) -> Result<E::Decision, HookFailure>>,
    fields: Vec<Field>,
    answers: Vec<Answer>,
}

/// A field's dotted path in the input, and what it holds.
type Field = (&'static str, &'static str);

fn speaks<E: Event, D: Dialect<E> + Copy>(
    name: &'static str,
    dialect: D,
    fields: &[&[Field]],
    answers: Vec<Answer>,
) -> Speaks<E> {
    Speaks {
        name,
        input: Box::new(move |payload| {
            let out = outgoing(&dialect, payload, Some(Path::new(DIR))).unwrap();
            serde_json::from_slice(&out.body).unwrap()
        }),
        env: dialect.env(Path::new(DIR)).vars().to_vec(),
        decide: Box::new(move |payload, reply| dialect.decide(payload, reply)),
        fields: fields.concat(),
        answers,
    }
}

/// One answer a hook may give, and what it does.
struct Answer {
    said: Said,
    does: &'static str,
}

enum Said {
    /// Stdout on exit 0, or a 2xx body.
    Out(&'static str),
    /// Exit 2, with this stderr.
    Exit2(&'static str),
}

const fn out(text: &'static str, does: &'static str) -> Answer {
    Answer {
        said: Said::Out(text),
        does,
    }
}

const fn exit2(stderr: &'static str, does: &'static str) -> Answer {
    Answer {
        said: Said::Exit2(stderr),
        does,
    }
}

/// One event's section.
struct Section<E: Event> {
    payload: E,
    fires: &'static str,
    note: &'static str,
    several: &'static str,
    dialects: Vec<Speaks<E>>,
}

fn session() -> Value {
    json!({ "id": "s-1", "transcriptPath": "/home/me/.local/share/haest/sessions/s-1.jsonl" })
}

/// A payload of `fields`, the session named.
fn payload<E: Event>(mut fields: Value) -> E {
    fields["session"] = session();
    serde_json::from_value(fields).unwrap()
}

/// A loop event's payload: the agent's view, then `own`.
fn looped<E: Event>(own: Value) -> E {
    let mut fields = json!({
        "modelSpec": "openrouter:anthropic/claude-sonnet-5.5",
        "effort": "high",
        "historyLength": 12,
        "contextTokens": 48_210,
        "contextWindow": 1_000_000,
    });
    if let (Some(fields), Value::Object(own)) = (fields.as_object_mut(), own) {
        fields.extend(own);
    }
    payload(fields)
}

/// An answer as a payload carries it, from the default model.
fn answer(mut m: AssistantMessage) -> Value {
    "openrouter".clone_into(&mut m.provider);
    "anthropic/claude-sonnet-5.5".clone_into(&mut m.model);
    serde_json::to_value(m).unwrap()
}

fn tool_call() -> Value {
    json!({ "name": "Bash", "input": { "command": "rm -rf build", "description": "Clean the build" }, "readOnly": false })
}

fn ctx() -> Value {
    json!({ "cwd": DIR, "toolCallId": "toolu_01" })
}

const SESSION_FIELDS: [(&str, &str); 2] = [
    (
        "session.id",
        "the session's id, as the host names it. `session` is left out when the host names none",
    ),
    (
        "session.transcriptPath",
        "the host's transcript of the session; left out when it keeps none",
    ),
];

const CALL_FIELDS: [(&str, &str); 5] = [
    ("call.name", "the tool, as the model called it"),
    (
        "call.input",
        "the input the tool will run with: the model's, or what the hooks before this one left",
    ),
    ("call.readOnly", "the tool declares it changes nothing"),
    ("ctx.cwd", "the conversation's working directory"),
    (
        "ctx.toolCallId",
        "the call's id; `null` when the loop gave it none",
    ),
];

const AGENT_FIELDS: [(&str, &str); 5] = [
    ("modelSpec", "the model in use, `provider:model`"),
    ("effort", "the effort asked for; `null` when none is"),
    ("historyLength", "how many messages the history holds"),
    (
        "contextTokens",
        "what the last model call put in the context window, in tokens; 0 before any. The number to compact on",
    ),
    (
        "contextWindow",
        "the model's context window in tokens, from the host's turn caps or the catalog; `null` when neither knows it",
    ),
];

const EVENT: (&str, &str) = ("event", "the event's name");

const CLAUDE_COMMON: [(&str, &str); 4] = [
    (
        "session_id",
        "the session's id; left out when the host names none",
    ),
    (
        "transcript_path",
        "haest's session file, not Claude Code's format; left out when the host keeps none",
    ),
    ("cwd", "the conversation's directory"),
    ("hook_event_name", "Claude Code's name for the event"),
];

const CLAUDE_TOOL: [(&str, &str); 3] = [
    ("tool_name", "the tool, as the model called it"),
    (
        "tool_input",
        "the input the tool will run with, as the hooks before this one left it",
    ),
    (
        "tool_use_id",
        "the call's id; left out when the loop gave it none",
    ),
];

fn tool_before() -> Section<ToolBeforePayload> {
    Section {
        payload: payload(json!({ "call": tool_call(), "ctx": ctx() })),
        fires: "before every tool call, a subagent's too",
        note: "",
        several: "Rewrites chain: each hook sees the input the one before left. The first deny wins, and no hook after it is asked.",
        dialects: vec![
            speaks(
                "haest",
                Haest,
                &[&[EVENT], &CALL_FIELDS, &SESSION_FIELDS],
                vec![
                    out("", "The call runs as it is."),
                    out(
                        r#"{"deny":"not on this machine"}"#,
                        "The call is refused and does not run. The model reads `Refused: not on this machine` as its result.",
                    ),
                    out(
                        r#"{"input":{"command":"rm -rf ./build","description":"Clean the build"}}"#,
                        "The call runs with this input, and the hooks after this one see it. It replaces the whole input: send every field you keep.",
                    ),
                    out(
                        r#"{"deny":null}"#,
                        "`null` counts as absent: the call runs.",
                    ),
                    out(
                        r#"{"deny":false}"#,
                        "A failure: `on_error`, by default `block`, refuses the call.",
                    ),
                    exit2(
                        "rm -rf is not allowed here",
                        "A command only: the call is refused, stderr the reason.",
                    ),
                ],
            ),
            speaks(
                "claude",
                Claude,
                &[&CLAUDE_COMMON, &CLAUDE_TOOL],
                vec![
                    out("", "The call runs as it is."),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not on this machine"}}"#,
                        "Refused, with the reason.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"rm -rf ./build"}}}"#,
                        "The call runs with this input: the whole input, as in Claude Code.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask"}}"#,
                        "Refused: a hook cannot ask the human for approval.",
                    ),
                    out(
                        r#"{"decision":"block","reason":"not on this machine"}"#,
                        "Claude Code's older form: refused.",
                    ),
                    out(
                        r#"{"continue":false,"stopReason":"stop everything"}"#,
                        "No haest decision: ignored, the call runs.",
                    ),
                    exit2("rm -rf is not allowed here", "Refused, stderr the reason."),
                ],
            ),
        ],
    }
}

fn tool_after() -> Section<ToolAfterPayload> {
    let mut call = tool_call();
    call["input"] = json!({ "command": "cargo test" });
    Section {
        payload: payload(json!({
            "call": call,
            "output": { "content": "test result: ok. 48 passed; 0 failed", "isError": false },
            "ctx": ctx(),
        })),
        fires: "after a tool call ran, before the model reads its output",
        note: "An output carries more fields when the tool set them: `title`, `result`, `media`, `costUsd`, `childCostUsd`.",
        several: "Each hook sees the output the one before left. What the call spent stays the tool's.",
        dialects: vec![
            speaks(
                "haest",
                Haest,
                &[
                    &[EVENT],
                    &CALL_FIELDS,
                    &[
                        ("output.content", "what the model reads"),
                        ("output.isError", "the call failed; `content` says why"),
                        (
                            "output.sized",
                            "the tool fit `content` to its own budget, so haest does not spill it to a file",
                        ),
                    ],
                    &SESSION_FIELDS,
                ],
                vec![
                    out("", "The model reads the output as it is."),
                    out(
                        r#"{"content":"48 passed (the log is in test.log)","isError":false}"#,
                        "This output replaces the call's: the model reads it, and the hooks after this one see it.",
                    ),
                    exit2(
                        "too late",
                        "`tool.after` has no blocking answer: a failure, and by default (`continue`) the output stays.",
                    ),
                ],
            ),
            speaks(
                "claude",
                Claude,
                &[
                    &CLAUDE_COMMON,
                    &CLAUDE_TOOL,
                    &[(
                        "tool_response",
                        "the output's text, as Claude Code sends a failed call's",
                    )],
                ],
                vec![
                    out("", "The model reads the output as it is."),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"Coverage fell to 71%."}}"#,
                        "Added after the output the model reads.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":"48 passed"}}"#,
                        "A new output: the model reads this text. An object is sent as its JSON.",
                    ),
                    out(
                        r#"{"decision":"block","reason":"The tests reached the network."}"#,
                        "The call already ran: the reason is added after the output.",
                    ),
                    exit2("too late", "A failure, and by default the output stays."),
                ],
            ),
        ],
    }
}

fn prompt_submit() -> Section<PromptSubmitPayload> {
    Section {
        payload: payload(json!({ "text": "fix the failing test" })),
        fires: "when the human sends a message, before the model sees it",
        note: "",
        several: "Rewrites chain: each hook sees the text the one before left.",
        dialects: vec![
            speaks(
                "haest",
                Haest,
                &[
                    &[EVENT],
                    &[("text", "the message, as the hooks before this one left it")],
                    &SESSION_FIELDS,
                ],
                vec![
                    out("", "The model gets the message as it is."),
                    out(
                        r#""fix the failing test\n\nThe CI log is in ci.log.""#,
                        "The model gets this text instead.",
                    ),
                    out(
                        "fix the failing test",
                        "A failure: a string answer is a JSON string, quoted. By default (`continue`) the message goes as it was.",
                    ),
                    exit2("no", "`prompt.submit` has no blocking answer: a failure."),
                ],
            ),
            speaks(
                "claude",
                Claude,
                &[
                    &CLAUDE_COMMON,
                    &[(
                        "prompt",
                        "the message, as the hooks before this one left it",
                    )],
                ],
                vec![
                    out("", "The model gets the message as it is."),
                    out(
                        "The CI log is in ci.log.",
                        "Plain text is added after the message.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"The CI log is in ci.log."}}"#,
                        "The same, as JSON.",
                    ),
                    out(
                        r#"{"decision":"block","reason":"no secrets in prompts"}"#,
                        "haest cannot refuse a prompt: a failure, and by default the message goes as it was.",
                    ),
                    exit2("no", "A failure."),
                ],
            ),
        ],
    }
}

fn system_prompt() -> Section<SystemPromptPayload> {
    Section {
        payload: payload(json!({})),
        fires: "before every model call, a subagent's too",
        note: "It runs before every model call, so its text is in every request: an answer that changes from one call to the next changes the system prompt, and the prompt cache is written again from there. Claude Code's `SessionStart` runs once per session; under haest a `SessionStart` script runs before every call.",
        several: "The texts are joined, a blank line between.",
        dialects: vec![
            speaks(
                "haest",
                Haest,
                &[&[EVENT], &SESSION_FIELDS],
                vec![
                    out("", "Nothing is added."),
                    out(
                        r#""The repo is on main. Run `make check` before you commit.""#,
                        "Appended to the system prompt.",
                    ),
                    out(
                        "false",
                        "Nothing is added: JavaScript's `cond && 'text'` answers this.",
                    ),
                    exit2("no", "`system.prompt` has no blocking answer: a failure."),
                ],
            ),
            speaks(
                "claude",
                Claude,
                &[&CLAUDE_COMMON],
                vec![
                    out("", "Nothing is added."),
                    out(
                        "The repo is on main.",
                        "Plain text is appended to the system prompt.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"The repo is on main."}}"#,
                        "The same, as JSON.",
                    ),
                    exit2("no", "A failure."),
                ],
            ),
        ],
    }
}

fn model_before() -> Section<ModelBeforePayload> {
    let last = serde_json::to_value(Message::user("fix the failing test")).unwrap();
    Section {
        payload: looped(json!({ "lastMessage": last })),
        fires: "before every model call, after the host's own `before_model_call`",
        note: "",
        several: "The first stop wins; every steer is kept; the first compact counts.",
        dialects: vec![speaks(
            "haest",
            Haest,
            &[
                &[EVENT],
                &AGENT_FIELDS,
                &[(
                    "lastMessage",
                    "the last message in the history, as the session file writes it less media bytes, thinking signatures and provider state; `null` in an empty history",
                )],
                &SESSION_FIELDS,
            ],
            vec![
                out("", "The call is made."),
                out(
                    r#"{"compact":{"instructions":"keep every file path"}}"#,
                    "haest compacts the conversation before the call. The instructions go after haest's own summary prompt; `true` asks for that alone.",
                ),
                out(
                    r#"{"steer":"Run the tests before you answer."}"#,
                    "Queued as a user message: it lands after this call's tool results, or after an answer with none, which then does not end the turn.",
                ),
                out(
                    r#"{"stop":"over this turn's budget"}"#,
                    "The turn ends before the call, the reason its error.",
                ),
                out(
                    r#"{"stop":false}"#,
                    "A failure: `stop` is text. By default (`continue`) the call is made.",
                ),
                exit2(
                    "over this turn's budget",
                    "A command only: the turn ends before the call.",
                ),
            ],
        )],
    }
}

fn model_after() -> Section<ModelAfterPayload> {
    let asked = tool_answer(&[("toolu_02", "Bash", json!({ "command": "cargo test" }))]);
    Section {
        payload: looped(json!({ "answer": answer(asked) })),
        fires: "when an answer lands, before its tool calls run",
        note: "",
        several: "The first stop wins, and the answer's calls do not run; every steer is kept.",
        dialects: vec![speaks(
            "haest",
            Haest,
            &[
                &[EVENT],
                &AGENT_FIELDS,
                &[(
                    "answer",
                    "the answer, as the session file writes it less media bytes, thinking signatures and provider state",
                )],
                &SESSION_FIELDS,
            ],
            vec![
                out("", "The answer's tool calls run."),
                out(
                    r#"{"steer":"Cite the file you read."}"#,
                    "Queued as a user message: it lands after this answer's tool results.",
                ),
                out(
                    r#"{"stop":"the answer calls a tool it must not"}"#,
                    "The turn ends; the answer's calls do not run.",
                ),
                exit2(
                    "the answer calls a tool it must not",
                    "A command only: the turn ends.",
                ),
            ],
        )],
    }
}

fn turn_end() -> Section<TurnEndPayload> {
    Section {
        payload: looped(json!({
            "stopReason": "end_turn",
            "error": null,
            "answer": answer(text_answer("All 48 tests pass.")),
            "continued": false,
        })),
        fires: "when a turn is about to end on its own",
        note: "A hook that keeps the turn going checks `continued` (`stop_hook_active` in `claude`): without that, a turn it holds runs on to its caps.",
        several: "The first continue wins; the first compact counts.",
        dialects: vec![
            speaks(
                "haest",
                Haest,
                &[
                    &[EVENT],
                    &AGENT_FIELDS,
                    &[
                        ("stopReason", "why the turn ends, as its word"),
                        (
                            "error",
                            "what went wrong; `null` for a turn that ended normally",
                        ),
                        (
                            "answer",
                            "the answer that ends the turn, as `model.after` has it; `null` when the history does not end in one",
                        ),
                        (
                            "continued",
                            "a `turn.end` decision already kept this turn going",
                        ),
                    ],
                    &SESSION_FIELDS,
                ],
                vec![
                    out("", "The turn ends."),
                    out(
                        r#"{"continue":"Run the tests and fix what fails."}"#,
                        "The turn goes on: the text goes to the model as a user message.",
                    ),
                    out(
                        r#"{"compact":true}"#,
                        "haest compacts as the turn ends, so the next turn starts small.",
                    ),
                    out(
                        r#"{"continue":1}"#,
                        "A failure: `continue` is text. By default the turn ends.",
                    ),
                    exit2(
                        "Run the tests and fix what fails.",
                        "A command only: the turn goes on, stderr the text.",
                    ),
                ],
            ),
            speaks(
                "claude",
                Claude,
                &[
                    &CLAUDE_COMMON,
                    &[
                        (
                            "stop_hook_active",
                            "a `turn.end` decision already kept this turn going",
                        ),
                        (
                            "last_assistant_message",
                            "the answer's text; left out when the history does not end in an answer",
                        ),
                    ],
                ],
                vec![
                    out("", "The turn ends."),
                    out(
                        r#"{"decision":"block","reason":"Run the tests and fix what fails."}"#,
                        "The turn goes on with the reason as a user message.",
                    ),
                    out(
                        r#"{"decision":"block"}"#,
                        "A failure: the turn would go on with no text. By default the turn ends.",
                    ),
                    out(
                        r#"{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"Run the tests."}}"#,
                        "The turn goes on with the text.",
                    ),
                    exit2(
                        "Run the tests and fix what fails.",
                        "The turn goes on, stderr the text.",
                    ),
                ],
            ),
        ],
    }
}

/// `text` safe in a markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// `text` in backticks, with enough of them for what it holds.
fn code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

/// `v` with every object's keys in order, so the page reads the same
/// whatever order serde_json keeps keys in: a build where a dependency turns
/// on its `preserve_order` (haest-acp's protocol crate does) keeps them in
/// the order they were inserted.
fn sorted(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut fields: Vec<(String, Value)> = map.into_iter().collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(fields.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// What haest made of an answer, as a cell.
fn reads<E: Event>(got: Result<E::Decision, HookFailure>) -> String {
    match got {
        Ok(decision) => {
            let passes = serde_json::to_value(E::Decision::default()).unwrap();
            let decision = serde_json::to_value(decision).unwrap();
            if decision == passes {
                "pass".into()
            } else {
                code(&sorted(decision).to_string())
            }
        }
        Err(failure) => format!("a failure: {failure}"),
    }
}

/// The dotted paths of `input` a field table must name: each key, or each
/// of an object's keys when the table names them and not the object.
fn paths(input: &Value, named: &BTreeSet<&str>, prefix: &str, out: &mut BTreeSet<String>) {
    let Some(fields) = input.as_object() else {
        return;
    };
    for (key, value) in fields {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let inside = named.iter().any(|n| n.starts_with(&format!("{path}.")));
        if value.is_object() && !named.contains(path.as_str()) && inside {
            paths(value, named, &path, out);
        } else {
            out.insert(path);
        }
    }
}

fn dialect_part<E: Event>(out: &mut String, event: ModEvent, payload: &E, d: &Speaks<E>) {
    let input = (d.input)(payload);
    let named: BTreeSet<&str> = d.fields.iter().map(|(path, _)| *path).collect();
    let mut has = BTreeSet::new();
    paths(&input, &named, "", &mut has);
    let named_owned: BTreeSet<String> = named.iter().map(|n| (*n).to_string()).collect();
    assert_eq!(
        has, named_owned,
        "{event} in {}: the field table must name exactly the input's fields",
        d.name
    );
    let pretty = serde_json::to_string_pretty(&sorted(input)).unwrap();
    let _ = write!(
        out,
        "\n### `{event}` in `{}`\n\n```json\n{pretty}\n```\n",
        d.name
    );
    if !d.env.is_empty() {
        let vars: Vec<String> = d
            .env
            .iter()
            .map(|(name, value)| format!("`{name}={value}`"))
            .collect();
        let _ = writeln!(out, "\nA command also gets {}.", vars.join(", "));
    }
    out.push_str("\n| Field | What |\n|---|---|\n");
    for (path, what) in &d.fields {
        let _ = writeln!(out, "| `{path}` | {} |", cell(what));
    }
    out.push_str("\n| The hook answers | haest reads | What happens |\n|---|---|---|\n");
    for a in &d.answers {
        let (said, reply) = match a.said {
            Said::Out("") => ("nothing".to_string(), Reply::Answer(String::new())),
            Said::Out(text) => (code(text), Reply::Answer(text.into())),
            Said::Exit2(stderr) => (
                format!("exit 2, stderr {}", code(stderr)),
                Reply::Blocked(stderr.into()),
            ),
        };
        let read = reads::<E>((d.decide)(payload.clone(), reply));
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            cell(&said),
            cell(&read),
            cell(a.does)
        );
    }
}

/// The event table's row for `E`.
fn row<E: Event>(out: &mut String, s: &Section<E>) {
    let event = E::NAME;
    let claude = match event.claude_code() {
        Some(name) => format!("`{name}`"),
        None => "none".into(),
    };
    let claude_binds = HookSettings::default()
        .with_entry(
            event,
            HookEntry::shell("x").with_dialect(HookDialect::Claude),
        )
        .is_ok();
    assert_eq!(
        claude_binds,
        event.claude_code().is_some(),
        "{event}: the claude dialect binds where Claude Code has the event"
    );
    let exit2 = match super::blocked::<E>(STDERR) {
        Ok(decision) => code(
            &sorted(serde_json::to_value(decision).unwrap())
                .to_string()
                .replace(&format!("\"{STDERR}\""), "<stderr>"),
        ),
        Err(_) => "a failure".into(),
    };
    let matcher = HookSettings::default()
        .with_entry(event, HookEntry::shell("x").with_matcher("Bash"))
        .is_ok();
    let _ = writeln!(
        out,
        "| [`{event}`](#{}) | {} | {claude} | {exit2} | `{}` | {} |",
        event.as_str().replace('.', ""),
        s.fires,
        E::ON_ERROR.as_str(),
        if matcher { "yes" } else { "no" },
    );
}

fn section<E: Event>(out: &mut String, s: &Section<E>) {
    let event = E::NAME;
    let _ = write!(out, "\n## `{event}`\n\nFires {}. {}\n", s.fires, s.several);
    if !s.note.is_empty() {
        let _ = write!(out, "\n{}\n", s.note);
    }
    for d in &s.dialects {
        dialect_part(out, event, &s.payload, d);
    }
}

/// The whole doc.
fn reference() -> String {
    let (tb, ta, ps, sp, mb, ma, te) = (
        tool_before(),
        tool_after(),
        prompt_submit(),
        system_prompt(),
        model_before(),
        model_after(),
        turn_end(),
    );
    let mut out = INTRO.to_string();
    out.push_str("\n| Event | Fires | In Claude Code | Exit 2 | `on_error` | `matcher` |\n|---|---|---|---|---|---|\n");
    row(&mut out, &tb);
    row(&mut out, &ta);
    row(&mut out, &ps);
    row(&mut out, &sp);
    row(&mut out, &mb);
    row(&mut out, &ma);
    row(&mut out, &te);
    let _ = write!(
        out,
        "\nExit 2 is a command's blocking answer: the decision it makes, its stderr the text (the model reads {TRUNCATE_CHARS} characters of it at most, cut in the middle past that, as a tool's output is). `on_error` is what a failure does unless the entry says. `matcher`: whether an entry may name the tools it is for.\n"
    );
    out.push_str(TRANSPORTS);
    out.push_str(OPTIONS_INTRO);
    let _ = write!(
        out,
        "\n```toml\n{EXAMPLE}```\n\n| Key | Takes | Default |\n|---|---|---|\n"
    );
    let timeout = format!("{}", DEFAULT_HOOK_TIMEOUT.as_secs());
    for (key, takes, default) in KEYS {
        let default = default.replace("{timeout}", &timeout);
        let _ = writeln!(out, "| `{key}` | {takes} | {default} |");
    }
    out.push_str(OPTIONS_AFTER);
    out.push_str(DIALECTS);
    section(&mut out, &tb);
    section(&mut out, &ta);
    section(&mut out, &ps);
    section(&mut out, &sp);
    section(&mut out, &mb);
    section(&mut out, &ma);
    section(&mut out, &te);
    out
}

#[test]
fn hooks_reference_is_what_the_dialects_write() {
    let doc = reference();
    if std::env::var_os("HAEST_BLESS").is_some() {
        std::fs::write(DOC, &doc).unwrap();
    }
    let on_disk = std::fs::read_to_string(DOC).unwrap_or_default();
    assert!(
        on_disk == doc,
        "docs/hooks.md is not what the dialects write: HAEST_BLESS=1 cargo test -p haest-bridges --features shell hooks_reference"
    );
}

#[test]
fn hooks_reference_covers_every_event() {
    let doc = reference();
    for event in ModEvent::ALL {
        assert!(doc.contains(&format!("\n## `{event}`\n")), "{event}");
    }
}

#[test]
fn hooks_reference_config_example_loads_and_uses_every_key() {
    let file: toml::Table = toml::from_str(EXAMPLE).unwrap();
    let hooks: HookSettings = file["hooks"].clone().try_into().unwrap();
    assert_eq!(hooks.entries().count(), 2);
    for (key, ..) in KEYS {
        assert!(EXAMPLE.contains(&format!("\n{key} = ")), "{key}");
    }
}
