//! An agent's conversation (I4): answers as markdown, thinking as a dim lane
//! that folds, each tool call a card. One component serves the Stream tab
//! (main) and the Agents tab (the agent picked there): which one it follows
//! is a prop.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::{fmt_ms, fmt_tokens, fmt_usd};
use crate::{
    model::{
        AgentPhase, CacheSplit, Conn, Item, ItemKind, MAIN, NoticeLevel, SessionModel, Stream,
        ToolCard, ToolStatus,
    },
    store::{Store, Tab},
};

/// The widest a line of the stream gets: a readable measure.
const MEASURE_REM: f32 = 52.;
/// How many lines of a tool's result show before "Show all".
const RESULT_LINES: usize = 3;

/// Which agent the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    /// Main, always: the Stream tab.
    Main,
    /// The one picked in the Agents tab.
    Picked,
}

struct Scroller {
    state: Entity<MessageScrollerState>,
    /// Each row's (uid, rev) as last laid out.
    seen: Vec<(u64, u64)>,
}

/// The reader's open/closed choices, by session and item.
#[derive(Debug, Default)]
pub struct Folds {
    open: HashMap<(String, u64), bool>,
}

impl Folds {
    fn is_open(&self, sid: &str, uid: u64, default: bool) -> bool {
        *self.open.get(&(sid.to_string(), uid)).unwrap_or(&default)
    }
}

/// One agent's stream: the Stream tab (main) or the Agents tab's detail.
pub struct StreamPanel {
    store: Entity<Store>,
    follow: Follow,
    scrollers: HashMap<(String, String), Scroller>,
    folds: Entity<Folds>,
    _observe: Subscription,
}

impl std::fmt::Debug for StreamPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamPanel")
            .field("follow", &self.follow)
            .finish_non_exhaustive()
    }
}

impl StreamPanel {
    /// The panel over `store`, showing the agent `follow` names.
    pub fn new(store: Entity<Store>, follow: Follow, cx: &mut Context<'_, Self>) -> Self {
        let observe = cx.observe(&store, |this, _, cx| this.sync(cx));
        Self {
            store,
            follow,
            scrollers: HashMap::new(),
            folds: cx.new(|_| Folds::default()),
            _observe: observe,
        }
    }

    fn shown(&self, cx: &App) -> Option<(String, String)> {
        let store = self.store.read(cx);
        let sid = store.selected.clone()?;
        let agent = match self.follow {
            Follow::Main => MAIN.to_string(),
            Follow::Picked => store.agent_of(&sid).to_string(),
        };
        Some((sid, agent))
    }

    /// Tell each list what changed: rows added or replaced, rows that grew.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let store = self.store.read(cx);
        let mut updates = Vec::new();
        self.scrollers.retain(|(sid, agent), scroller| {
            let Some(stream) = store.session(sid).and_then(|s| s.stream(agent)) else {
                return false;
            };
            let now: Vec<(u64, u64)> = stream.items.iter().map(|i| (i.uid, i.rev)).collect();
            if now != scroller.seen {
                updates.push((scroller.state.clone(), scroller.seen.clone(), now.clone()));
                scroller.seen = now;
            }
            true
        });
        for (state, before, now) in updates {
            state.update(cx, |state, cx| {
                let same = before
                    .iter()
                    .zip(&now)
                    .take_while(|(a, b)| a.0 == b.0)
                    .count();
                if same < before.len() || same < now.len() {
                    state.splice(same..before.len(), now.len() - same, cx);
                }
                for (ix, (a, b)) in before.iter().zip(&now).enumerate().take(same) {
                    if a.1 != b.1 {
                        state.remeasure_items(ix..ix + 1, cx);
                    }
                }
            });
        }
        cx.notify();
    }

    fn scroller(
        &mut self,
        sid: &str,
        agent: &str,
        cx: &mut Context<'_, Self>,
    ) -> Entity<MessageScrollerState> {
        let key = (sid.to_string(), agent.to_string());
        if let Some(s) = self.scrollers.get(&key) {
            return s.state.clone();
        }
        let seen: Vec<(u64, u64)> = self
            .store
            .read(cx)
            .session(sid)
            .and_then(|s| s.stream(agent))
            .map(|s| s.items.iter().map(|i| (i.uid, i.rev)).collect())
            .unwrap_or_default();
        let count = seen.len();
        let state = cx.new(|cx| MessageScrollerState::new(count, cx));
        self.scrollers.insert(
            key,
            Scroller {
                state: state.clone(),
                seen,
            },
        );
        state
    }
}

impl Render for StreamPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some((sid, agent)) = self.shown(cx) else {
            return div().into_any_element();
        };
        let scroller = self.scroller(&sid, &agent, cx);
        let store = self.store.clone();
        let folds = self.folds.clone();
        let panel = cx.weak_entity();
        let s = store.read(cx);
        let Some(session) = s.session(&sid) else {
            return div().into_any_element();
        };
        let items = session.stream(&agent).map_or(0, |s| s.items.len());
        let banner = lost_banner(session, s.is_listed(&sid), store.clone(), cx);
        let body = if items == 0 {
            empty_state(session, &agent, cx).into_any_element()
        } else {
            let (row_sid, row_agent) = (sid.clone(), agent.clone());
            MessageScroller::new(
                SharedString::from(format!("stream-{sid}-{agent}")),
                scroller,
                move |ix, window, cx| {
                    row(&store, &folds, &panel, &row_sid, &row_agent, ix, window, cx)
                },
            )
            .with_list_style(StyleRefinement::default().px_4().py_3())
            // One step of the spacing scale between rows (the default is 2rem).
            .with_row_style(StyleRefinement::default().pb_3())
            .with_bottom_fade(cx.theme().background)
            .size_full()
            .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .children(banner)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

/// The socket went away: say so over the stream, which stays readable.
fn lost_banner(
    session: &SessionModel,
    listed: bool,
    store: Entity<Store>,
    cx: &App,
) -> Option<AnyElement> {
    let Conn::Lost(why) = &session.conn else {
        return None;
    };
    let t = cx.theme();
    let sid = session.sid.clone();
    let why = capitalize(why);
    let what = if listed {
        "The connection to this haest was lost."
    } else {
        "This haest has ended: its socket is gone."
    };
    Some(
        h_flex()
            .flex_none()
            .gap_3()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(t.border)
            .bg(t.danger.opacity(0.08))
            .child(Icon::new(IconName::Unplug).small().text_color(t.danger))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().child(what))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(format!("{why}. What arrived before stays below.")),
                    ),
            )
            .when(listed, |el| {
                el.child(
                    Button::new("reconnect")
                        .small()
                        .outline()
                        .icon(IconName::RefreshCw)
                        .label("Reconnect")
                        .on_click(move |_, _, cx| {
                            store.update(cx, |s, cx| {
                                s.reconnect(&sid);
                                cx.notify();
                            });
                        }),
                )
            })
            .into_any_element(),
    )
}

fn empty_state(session: &SessionModel, agent: &str, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let (title, line) = match session.conn {
        Conn::Connecting => (
            "Connecting",
            format!("Asking {} for its stream.", session.short_id()),
        ),
        _ if agent != MAIN => (
            "Nothing from this agent yet",
            "Its calls and answers show here as they stream.".to_string(),
        ),
        _ => (
            "Nothing has happened in this conversation yet",
            if session.can_command() {
                "Send it something below, or wait for its own terminal.".to_string()
            } else {
                "Its turns show here as they run.".to_string()
            },
        ),
    };
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .when(session.conn == Conn::Connecting, |el| {
            el.child(Spinner::new())
        })
        .child(div().text_sm().text_color(t.foreground).child(title))
        .child(div().text_xs().text_color(t.muted_foreground).child(line))
}

#[expect(
    clippy::too_many_arguments,
    reason = "a card's parts, each its own value"
)]
fn row(
    store: &Entity<Store>,
    folds: &Entity<Folds>,
    panel: &WeakEntity<StreamPanel>,
    sid: &str,
    agent: &str,
    ix: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let item = store
        .read(cx)
        .session(sid)
        .and_then(|s| s.stream(agent))
        .and_then(|s: &Stream| s.items.get(ix))
        .cloned();
    let Some(item) = item else {
        return div().into_any_element();
    };
    let el = render_item(&item, store, folds, panel, sid, agent, ix, window, cx);
    div()
        .w_full()
        .max_w(rems(MEASURE_REM))
        .child(el)
        .into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "a card's parts, each its own value"
)]
fn render_item(
    item: &Item,
    store: &Entity<Store>,
    folds: &Entity<Folds>,
    panel: &WeakEntity<StreamPanel>,
    sid: &str,
    agent: &str,
    ix: usize,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let t = cx.theme().clone();
    let uid = item.uid;
    let toggle = {
        let (folds, panel, sid, agent) = (
            folds.clone(),
            panel.clone(),
            sid.to_string(),
            agent.to_string(),
        );
        move |default: bool| {
            let (folds, panel, sid, agent) =
                (folds.clone(), panel.clone(), sid.clone(), agent.clone());
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                folds.update(cx, |f, _| {
                    let open = f.is_open(&sid, uid, default);
                    f.open.insert((sid.clone(), uid), !open);
                });
                let _ = panel.update(cx, |p, cx| {
                    if let Some(s) = p.scrollers.get(&(sid.clone(), agent.clone())) {
                        s.state.update(cx, |s, cx| {
                            s.remeasure_items(ix..ix + 1, cx);
                        });
                    }
                    cx.notify();
                });
            }
        }
    };
    match &item.kind {
        ItemKind::User { text } => v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child("Input"),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .rounded(t.radius)
                    .bg(t.secondary)
                    .text_sm()
                    .whitespace_normal()
                    .child(text.clone()),
            )
            .into_any_element(),
        ItemKind::Answer { text, .. } => TextView::markdown(("answer", uid), text.clone())
            .selectable(true)
            .text_sm()
            .into_any_element(),
        ItemKind::Thinking { text, live } => {
            let open = folds.read(cx).is_open(sid, uid, *live);
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .id(("thinking", uid))
                        .gap_1()
                        .px_1()
                        .py_0p5()
                        .rounded(t.radius)
                        .cursor_pointer()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .hover(|el| el.bg(t.secondary))
                        .active(|el| el.bg(t.secondary_active))
                        .on_click(toggle(*live))
                        .child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall(),
                        )
                        .child("Thinking")
                        .child(
                            div()
                                .text_color(t.muted_foreground.opacity(0.7))
                                .child(if *live {
                                    "streaming".to_string()
                                } else {
                                    format!("{} chars", fmt_tokens(text.chars().count() as u64))
                                }),
                        ),
                )
                .when(open, |el| {
                    el.child(
                        div()
                            .ml_2()
                            .pl_3()
                            .border_l_2()
                            .border_color(t.border)
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .whitespace_normal()
                            .child(text.clone()),
                    )
                })
                .into_any_element()
        }
        ItemKind::Tool(card) => {
            let open = folds.read(cx).is_open(sid, uid, false);
            // The approval ask names the call it holds up.
            let held = card.status == ToolStatus::Running
                && store
                    .read(cx)
                    .session(sid)
                    .is_some_and(|s| s.holds(&card.call_id));
            tool_card(card, uid, open, held, toggle(false), &t).into_any_element()
        }
        ItemKind::Notice { level, text } => {
            let (icon, color) = match level {
                NoticeLevel::Info => (IconName::Info, t.muted_foreground),
                NoticeLevel::Warning => (IconName::TriangleAlert, t.warning),
                NoticeLevel::Error => (IconName::OctagonX, t.danger),
            };
            let text_color = match level {
                NoticeLevel::Info => t.muted_foreground,
                _ => t.foreground,
            };
            line(icon, color, text_color, text.clone()).into_any_element()
        }
        ItemKind::Guard { text } => line(
            IconName::Hand,
            t.warning,
            t.foreground,
            format!("Guard: {text}"),
        )
        .into_any_element(),
        ItemKind::Agent {
            id,
            name,
            description,
            phase,
        } => {
            let verb = match phase {
                AgentPhase::Started => "running",
                AgentPhase::Finished => "finished",
                AgentPhase::StoppedEarly => "stopped early",
            };
            let color = match phase {
                AgentPhase::StoppedEarly => t.warning,
                _ => t.muted_foreground,
            };
            let (store, sid, id) = (store.clone(), sid.to_string(), id.clone());
            h_flex()
                .id(("agent", uid))
                .gap_2()
                .px_1()
                .py_0p5()
                .rounded(t.radius)
                .cursor_pointer()
                .text_xs()
                .hover(|el| el.bg(t.secondary))
                .active(|el| el.bg(t.secondary_active))
                .on_click(move |_, _, cx| {
                    store.update(cx, |s, cx| {
                        s.select_agent(&sid, &id);
                        s.tab = Tab::Agents;
                        cx.notify();
                    });
                })
                .child(Icon::new(IconName::Bot).xsmall().text_color(color))
                .child(
                    div()
                        .text_color(t.foreground)
                        .child(format!("{name} {verb}")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(t.muted_foreground)
                        .child(description.clone()),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .xsmall()
                        .text_color(t.muted_foreground),
                )
                .into_any_element()
        }
        ItemKind::TurnEnd(end) => {
            let mut parts = vec![
                end.stop_reason.replace('_', " "),
                fmt_ms(end.duration_ms),
                format!(
                    "{} in · {} out",
                    fmt_tokens(end.usage.input + end.usage.cache_read + end.usage.cache_write),
                    fmt_tokens(end.usage.output)
                ),
            ];
            if let Some(split) = CacheSplit::of(&end.usage) {
                parts.push(format!("{:.0}% cached", split.served * 100.));
            }
            if let Some(usd) = end.usage.cost_usd {
                parts.push(fmt_usd(usd));
            }
            v_flex()
                .gap_1()
                .pt_2()
                .border_t_1()
                .border_color(t.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(format!("Turn ended: {}", parts.join(" · "))),
                )
                .when_some(end.error.clone(), |el, e| {
                    el.child(div().text_xs().text_color(t.danger).child(e))
                })
                .into_any_element()
        }
        ItemKind::Lost { missed, caught_up } => {
            let n = missed.map_or("Some".to_string(), |n| n.to_string());
            let text = if *caught_up {
                format!(
                    "{n} frames were lost: everything above is the conversation as its history has it, without timings."
                )
            } else {
                format!("{n} frames were lost. Catching up from its history.")
            };
            line(IconName::TriangleAlert, t.warning, t.foreground, text).into_any_element()
        }
    }
}

fn line(
    icon: IconName,
    color: gpui::Hsla,
    text_color: gpui::Hsla,
    text: String,
) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_start()
        .text_xs()
        .child(
            div()
                .pt_0p5()
                .child(Icon::new(icon).xsmall().text_color(color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(text_color)
                .whitespace_normal()
                .child(text),
        )
}

fn tool_card(
    card: &ToolCard,
    uid: u64,
    open: bool,
    held: bool,
    toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    t: &gpui_component::Theme,
) -> impl IntoElement {
    let (status, failed) = match &card.status {
        ToolStatus::Running if held => (
            Icon::new(IconName::ShieldAlert)
                .xsmall()
                .text_color(t.warning)
                .into_any_element(),
            false,
        ),
        ToolStatus::Running => (
            Spinner::new()
                .xsmall()
                .color(t.muted_foreground)
                .into_any_element(),
            false,
        ),
        ToolStatus::Done { error: true, .. } => (
            Icon::new(IconName::OctagonX)
                .xsmall()
                .text_color(t.danger)
                .into_any_element(),
            true,
        ),
        ToolStatus::Done { .. } => (
            Icon::new(IconName::Check)
                .xsmall()
                .text_color(t.success)
                .into_any_element(),
            false,
        ),
    };
    let took = match &card.status {
        ToolStatus::Done { ms: Some(ms), .. } => fmt_ms(*ms),
        ToolStatus::Done { ms: None, .. } => String::new(),
        ToolStatus::Running if card.input.is_none() => "writing input".into(),
        ToolStatus::Running if held => "waiting for approval".into(),
        ToolStatus::Running => "running".into(),
    };
    // A command's output ends in a newline; it is not a line to show.
    let result = card
        .result
        .as_deref()
        .unwrap_or_default()
        .trim_end()
        .to_string();
    let lines: Vec<&str> = result.lines().collect();
    let long = lines.len() > RESULT_LINES;
    let shown = if open || !long {
        result.clone()
    } else {
        lines[..RESULT_LINES].join("\n")
    };
    let title = card.title.clone().unwrap_or_else(|| card.preview.clone());
    v_flex()
        .rounded(t.radius)
        .border_1()
        .border_color(if failed {
            t.danger.opacity(0.5)
        } else {
            t.border
        })
        .bg(t.group_box)
        .child(
            h_flex()
                .gap_2()
                .px_3()
                .py_1p5()
                .text_xs()
                .child(status)
                .child(
                    div()
                        .flex_none()
                        .font_family(t.mono_font_family.clone())
                        .font_semibold()
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(t.mono_font_family.clone())
                        .text_color(t.muted_foreground)
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(if failed {
                            t.danger
                        } else if held {
                            t.warning
                        } else {
                            t.muted_foreground
                        })
                        .child(if failed {
                            format!("error · {took}")
                        } else {
                            took
                        }),
                ),
        )
        .when(!result.is_empty(), |el| {
            el.child(
                v_flex()
                    .px_3()
                    .py_1p5()
                    .gap_1()
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .text_xs()
                            .font_family(t.mono_font_family.clone())
                            .text_color(t.muted_foreground)
                            .whitespace_normal()
                            .child(shown),
                    )
                    .when(long, |el| {
                        el.child(
                            h_flex().child(
                                Button::new(("tool-more", uid))
                                    .xsmall()
                                    .ghost()
                                    .label(if open {
                                        "Show less".to_string()
                                    } else {
                                        format!("Show all {} lines", lines.len())
                                    })
                                    .on_click(toggle),
                            ),
                        )
                    }),
            )
        })
}

/// `the socket closed` -> `The socket closed`.
fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
