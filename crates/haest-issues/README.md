# haest-issues

**Experimental, and an example of how to write tools for haest.** The API may
change in any release.

Issue-tracker tools for a [haest](https://github.com/haest-werk/haest) agent:
list, search, get, create, update, delete and link issues, over one trait with
two backends.

- `Tracker`: the contract the tools talk to. Implement it for another tracker
  and every tool works on it unchanged.
- `GithubTracker`: the issues of one GitHub repository, over the REST API
  (deleting an issue is the one GraphQL call; REST has no endpoint for it).
- `CardTracker`: board cards, one markdown file per card with its fields in
  YAML frontmatter, in a directory you name. It reads and writes the card
  files of remote-claude's project board (`.rclaude/project/cards/`) byte for
  byte as that board does.
- `issue_tools(tracker)`: the seven haest tools over any tracker.

## Use

```rust
use std::sync::Arc;
use haest_issues::{cards::CardTracker, github::GithubTracker, issue_tools, issue_tools_named};
use haest_workspace::RealFs;

// GitHub: the token is yours to hand in; nothing is read from the environment.
let github = GithubTracker::new("owner", "repo").with_token(token);
let mut tools = issue_tools(Arc::new(github));

// Cards: you name the directory and the file system.
let board = CardTracker::new(Arc::new(RealFs), "/path/to/project/cards");
tools.extend(issue_tools_named(Arc::new(board), "Card")); // CardList, CardGet, ...

// Hand `tools` to the agent with its others (AgentDeps::new(models, Arc::new(tools), ...)).
```

`cargo run -p haest-issues --example issues` runs a scripted agent that plans
work on a board in a temp dir (no network, no keys); add `-- --github owner/repo`
to also list a public repository's open issues through the same tools, read
only and without a token.

## The tools

| Tool | Kind | Read only | Does |
|---|---|---|---|
| IssueList | `issue.list` | yes | issues by status (the open ones by default, or `all`), labels, priority |
| IssueSearch | `issue.search` | yes | issues by words, any status |
| IssueGet | `issue.get` | yes | one issue in full, with its links from both sides |
| IssueCreate | `issue.create` | no | a new issue |
| IssueUpdate | `issue.update` | no | title, body, status, priority; `labels` replaces the set, `add_labels` / `remove_labels` change some |
| IssueDelete | `issue.delete` | no | deletes for good |
| IssueLink | `issue.link` | no | adds or removes a relation: `parent`, `child`, `depends_on`, `blocks`, `relates_to` |

Each tool's description and schema are written from what its tracker accepts:
the statuses, the priorities (left out when there are none) and the relations.

**IssueDelete is the destructive one.** haest's tool contract has no
"destructive" flag: a tool states `read_only` (false for every writer here) and
its canonical `kind`. IssueDelete's kind is `issue.delete`
(`haest_issues::DELETE_KIND`), and its description tells the model it cannot be
undone and to change the status instead unless the human asked. A host that
asks the human before irreversible calls should gate this tool by that kind;
haest's built-in approval gate covers Bash only.

## GitHub and cards, side by side

| | GitHub | Cards |
|---|---|---|
| id | the issue number (`42`; `owner/repo#42` names another repository's issue in a link) | the file name without `.md`, made from the title at create and never changed |
| statuses | `open`, `closed`, `not_planned` (`state` + `state_reason`) | `inbox`, `open`, `in-progress`, `in-review`, `done`, `archived` (`status:`; the file never moves) |
| priority | **none**: GitHub issues have no priority field | `low`, `medium`, `high` |
| labels | labels | `tags:` |
| parent / child | sub-issues (a new parent replaces the old one) | `epic:` on the child |
| depends_on / blocks | issue dependencies ("blocked by" / "blocking") | `depends_on:` on the card that waits |
| relates_to | **none**: GitHub has no such link; the tool is not offered it | `relates_to:` (read from both cards) |
| delete | GraphQL `deleteIssue` (needs a token with admin rights on the repository) | the file is removed |

Not mapped: GitHub's author, assignees, milestone and comment count are shown
by IssueGet, but no tool changes them, and comments, issue types and project
fields are not read at all. A card's `refs:`, `quest:` and every other key are
shown by IssueGet and kept on every write, but no tool edits them.

## Card files

```markdown
---
title: "Index the titles"
status: in-progress
priority: high
tags: [search]
epic: search-v2
created: 2026-11-16T12:00:00.000Z
---

Titles first, bodies next.
```

- The store's keys are written first, in this order: `title`, `status`,
  `priority`, `tags`, `refs`, `quest`, `epic`, `depends_on`, `relates_to`,
  `created`, then the board's other known keys. **Every other key survives a
  write**, in the order the card had it; a block the flat subset cannot
  represent (a nested mapping, a `|` scalar) is kept verbatim.
- A block list under a list key (`refs:` then `  - a`) is written back inline,
  and `blocked_by` / `see_also` are folded into `depends_on` / `relates_to`, as
  the board's own writer does.
- A write goes to a hidden temp file in the directory, then is renamed over the
  card. A link must name a card that exists, and a link that would make a loop
  (`depends_on`, `epic`) is refused.
- One `CardTracker` writes one card at a time, but two trackers over the same
  directory (or another program) do not wait for each other: two writes at the
  same moment can lose a change, or make two same-titled cards one. Make one
  tracker per directory and share it.

## Writing your own tools

This crate is meant to be read. `src/tools.rs` and `src/tools/` show a family
of haest tools over one shared backend: a name, a description and a JSON Schema
built from what the backend accepts, a canonical `kind`, `read_only` for the
readers, a correctable mistake returned as `ToolError::Input` and anything else
as `ToolError::Failed`, and every call given up when the turn is cancelled.
`GithubTracker::with_http` is a plug point in haest's style: the backend sends
through haest-http's one-method `HttpClient` (re-exported as
`haest_issues::http`), with a reqwest default and a fake in the tests.
