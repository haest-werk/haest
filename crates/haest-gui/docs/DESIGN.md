# haest-gui: design note

No comp exists. This note is the comp: every view, what each element on it is
for, and every state. An element without a line here does not ship.

## Ground rules

- Desktop tool density. Base font 14 px (the theme's `font_size`, so `rem` =
  14 px); every size and gap comes from the rem helpers or a component size.
  Controls are `small` in toolbars and rows, default elsewhere.
- One spacing scale: `1` (parts of one control), `2` (related controls, row
  padding), `3` (one content group), `4` (panel padding, separate groups),
  `6` (separate sections). Nothing else.
- Dark theme from gpui-component's theme system (`Theme::change(Dark)`).
  Colors only by semantic role: `background`/`foreground`, `muted_foreground`
  for metadata, `border` for structure, `primary` for the one primary action
  and selection, `danger`/`warning`/`success` only for their meanings, the
  `chart_*` tokens for the timeline's data. No raw colors.
- One primary action per view. Every interactive element has hover, focus,
  active and disabled states (gpui-component's controls have them; custom
  rows get `hover`/`active` backgrounds and a focus ring).
- Monospace only for identifiers, commands, tool input and numbers that line up.

## The shell (main window)

Sidebar workspace: a session list on the leading edge, the attached session on
the rest.

| Element | Why it is there |
|---|---|
| Sidebar: "Sessions" heading | names the list |
| Session row | one haest with a control socket on this machine. Shows its model (what it is), the short stream id (which one, mono), its access (`watch`/`command`: what haest-gui may do with it), a state dot (running/idle/ended: is it working) and, when attached, an attached mark. Click: attach and show it |
| Row detach button (attached rows only) | stop following it; its asks leave the approval queue. The row's text truncates before it |
| Title bar, trailing edge: approvals button | opens the approval panel (Cmd+Shift+A); warning-coloured "N waiting" when asks wait across every attached haest. In the title bar because it belongs to the app, not to one session |
| Header: session title | which haest this is: model, a state dot and word; under it the short id, access, cost (once there is any) and message count |
| Header tabs: Stream, Timeline, Agents | the three ways to read one session |
| Header: model select, effort select (command only) | change the model or effort from the next turn on (the tooltip says so) |
| Header: Interrupt (command only) | stop the running turn; disabled when idle, with a tooltip saying why |

Narrow window (under 760 px): the sidebar is hidden and a sessions button in the
header opens the same list in a sheet. The header's selects shrink before the
title truncates.

## Stream (I4)

The conversation of the selected agent (main unless one was picked in Agents).

| Element | Why |
|---|---|
| User block | the input that started or steered the turn |
| Answer | the model's text, rendered as markdown |
| Thinking lane | the model's reasoning: dim, smaller, collapsible (a header with a chevron and its length). Open while it streams, closed once the answer is in, then the reader's choice |
| Tool card | one tool call: name (mono), a one-line input preview (the command, the path, or compact JSON), status ("writing input" while the model still writes the call; spinner and "running" while it runs; shield and "waiting for approval" in warning while an approval ask names this call by its tool call id and nobody has answered it; its duration when done; `error` in danger when it failed), and the result: three lines (a trailing newline is not a line), "Show all" under them to open it |
| Notice / guard line | haest's own notices and the guard's nudges, at their level; info notices muted. haest's "Subagents running: N" is left out: the subagent line and the Agents tab say it |
| Answered line | how an approval ask ended and where: "Bash: denied at its terminal", "Bash: allowed here", "Bash: denied by another program", from haest's `approval_answered` frame. The card above it says what the tool then did; this line says who decided. "here" means haest-gui had sent an answer: the wire names the side (terminal or peer), not which program |
| Subagent line | one row per subagent, its phase (running, finished, stopped early) moving on in place; click opens it in Agents |
| Turn footer | how the turn ended, its time, tokens, cache share and cost |
| Jump to latest | appears only when scrolled away from the live edge |
| Composer (command only) | the view's one primary action: send input. Enter sends, Shift+Enter is a newline. While a turn runs the placeholder says the text joins the running turn |

States: empty (attached, no frames yet: "Nothing has happened in this
conversation yet"), connecting (spinner, "Connecting", "Asking <id> for its
stream."), error (a banner over the stream: the socket went away and why; the
stream that arrived stays readable; Reconnect when the haest is still listed,
which rebuilds the session from its first frame so an ask it still waits on
comes back), long content (the
list is virtualized; long tool results fold to three lines), gap (a line
saying frames were lost and the conversation above came from `history`).

## Timeline (I1)

Each turn of main is a track (a subagent's work shows as its Agent tool bar;
its own calls are in Agents). Time runs left to right from the turn's start, so
turns compare. Model calls sit on the first lane; tool calls below it, and the
calls of one batch stack, one lane each. A track is at least two lanes tall so
its label fits.

| Element | Why |
|---|---|
| Track label (leading column) | line one: the turn number, and how it ended when that was not the model finishing (`running`, `aborted`); line two: its duration and cost |
| Model call bar | a model call from `model_start` to `assistant`, split by what its input was: served from the prompt cache (`chart_1`, light blue: the cheap part), written to it (`warning`, yellow: a one-time premium), sent uncached (`chart_3`, blue), from the `assistant` frame's own usage, per call. A call that ended with no usage is a faint fill; its tooltip says so |
| Tool call bar | a tool call from its batch start to its answer (`secondary`); danger edge when it failed. A bar too narrow for its name shows the name after it, muted |
| Running bar | a call still running grows to now |
| Time axis | ticks that re-space as you zoom |
| Legend | what the three cache shades mean |
| Zoom readout ("8.9 s across") + Fit | how much time the plot shows; Fit returns to the whole turn set (disabled when already fitted) |

Zoom: pinch, or Cmd/Ctrl + scroll, anchored at the pointer. Pan: horizontal
scroll, or drag. Hover a bar for its details. States: empty ("No turns yet"),
history-only turns (from a gap: no timings, the track says so), running turn,
long sessions (tracks scroll vertically), narrow (the label column shrinks).

## Approval panel (I3)

A small always-on-top window (`WindowKind::PopUp`). Asks from every attached
haest land in one queue, oldest first. A new ask opens it without taking focus.

| Element | Why |
|---|---|
| Ask card | line one: the tool (mono) and which haest asks (model + short id, muted), and "at its terminal too" when the same question waits at that haest's own terminal (the first answer counts, wherever it comes from); then why it asks, in haest's words; then its input preview (mono; a shell command whole) |
| Selected row | the one the keyboard answers |
| Allow (primary) / Always (outline) / Deny (ghost, apart) | answer this ask; tooltips give the keys, and Always says exactly what it will allow from now on. Once sent the buttons give way to "Sent: allow. Waiting for the haest." |
| Terminal ask row (read-only) | a question only its own terminal can answer (a haest with a watch socket, or the `ask` tool): haest-gui shows it, says to answer it there, and drops it at that haest's turn end (the wire has no frame for its answer) |

Keys: Up/Down select, Enter allow, Cmd+Enter always, Delete deny. The panel never
takes focus by itself (an Enter typed elsewhere must never approve anything),
and a new ask ignores keys for its first 400 ms. An approval ask leaves the
queue the moment the wire ends it: `approval_answered` (answered here, at its
terminal, or by another program) or `approval_withdrawn`. It does not leave at
a turn end: a background subagent can still be waiting on it. A terminal's
`ask` line that names an approval ask (`approval: N`) is that same ask, so it
adds no second row. A lost connection takes its asks out of the queue (nothing
can answer them); Reconnect brings back the ones still waiting. States: empty ("No approvals waiting"), several sessions,
long previews (wrap, capped, scroll), watch sockets (their asks show
read-only: answering needs command access).

## Agents (I5)

Master-detail: the agent tree on the leading side, the selected agent's stream
on the rest.

| Element | Why |
|---|---|
| Tree node | main, and each subagent under the agent that started it. Line one: its name (mono, the row's whole width); line two: its state (running, done in N s, stopped early) and what it was asked, muted. Main says "the conversation" |
| State mark | green dot while it works, pulsing while a model call streams (steady when the system asks for reduced motion); a check when finished; a warning triangle when stopped early |
| Detail header | the selected agent's name, its state and what it was asked |
| Selected node | whose stream the detail shows |

States: no subagents (main alone, with a line saying subagents appear here
when the model starts one), running, finished, many (the tree scrolls).
