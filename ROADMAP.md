# Roadmap

What haest does not do yet, and what might come. Decisions and their reasons are in
[PLAN.md](PLAN.md); what each release changed is in [CHANGELOG.md](CHANGELOG.md).

## Known gaps

Scheduled work: each is a gap in what haest does today.

| Gap | Today |
|---|---|
| The auto-mode judge | the built-in approvals ask the human before a shell command that cannot be undone and let the rest run; no model judges a call on its own |
| A resumed sandbox session | the overlay lives in the process: a resumed session starts from a clean one, and edits not applied before exit are gone |
| MCP and compaction, live | both pass their tests; neither has run against a live server or a real long session in this repo's checks |
| `mv` of a directory across sandbox mounts | fails |
| A PDF from an MCP tool | stays text; it does not become media |
| The `claude` profile | reads part of Claude Code's setup. It does not read Claude Code's agents, commands, permissions, user or plugin MCP servers, `@` imports, rules, memory or output styles. Claude Code's hook scripts run when you bind them with the `hooks` setting (`dialect = "claude"`), not from `.claude/settings.json` |
| Durable runs | keep their records in memory only: no store that outlives the process (JSONL, SQLite), no durable subagents, no resumable `parallel` / `pipeline` flows, no `durable` setting |
| Tool costs | a price per tool call (charged when it starts, settled when it ends) is not built |
| Remote agents | remote subagents, haest driving another agent through a child or WebSocket backend, and a broker server are not built |

The cases haest leaves alone on purpose are the README's
[Limitations](README.md#limitations): handling each would cost more than the case is worth.

## Ideas, not scheduled

Ideas for long-running agents. None of them is planned yet. Each would build on the extension
points that exist: the loop events, `turn.end`'s `continue`, the cache's idle tick.

| Idea | What it would do |
|---|---|
| Triggers at the end of a turn | start the next turn from an event (a job's output, a file, a message) with no human turn in between |
| Schedules | wake a conversation on a cron expression |
| Timeouts and watchdogs | notice a turn or a conversation that stalls, and act on it |
| `sleep(2h, keep_cache_alive: true)` | a tool the model calls to pause, its cache kept warm while it waits |
| `wakeup_on_new_context(instructions, "1h 30m")` | wake later in a fresh context, with a summary and ways to query the old history |
| A monitor tool | the model watches a process or a file and is woken by what it sees |
| Compaction as a tool | the model compacts its own conversation, so the next turn starts small |
| A changed system prompt that keeps the cache | when a mod changes its `system.prompt` text, send the change as a mid-conversation system message instead of a new system prompt, so the prompt cache survives |
