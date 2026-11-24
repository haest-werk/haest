//! The subagent map (I5): main and its subagents as a live tree, a node
//! pulsing while its agent streams; picking one shows its stream beside it.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AppContext as _, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _, pulsating_between, rems,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex, scroll::ScrollableElement as _, v_flex,
};
use gpui_kit_assets::IconName;

use super::{
    fmt_ms,
    stream::{Follow, StreamPanel},
};
use crate::{
    model::{AgentNode, AgentState, MAIN},
    store::Store,
};

const TREE_REM: f32 = 17.;

/// The Agents tab: the subagent tree beside the picked agent's own stream.
pub struct AgentsView {
    store: Entity<Store>,
    detail: Entity<StreamPanel>,
    _observe: Subscription,
}

impl std::fmt::Debug for AgentsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentsView").finish_non_exhaustive()
    }
}

impl AgentsView {
    /// The view over `store`.
    pub fn new(store: Entity<Store>, cx: &mut Context<'_, Self>) -> Self {
        let detail = cx.new(|cx| StreamPanel::new(store.clone(), Follow::Picked, cx));
        let observe = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            detail,
            _observe: observe,
        }
    }
}

impl Render for AgentsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let narrow = f32::from(window.viewport_size().width) < 900.;
        let store = self.store.read(cx);
        let Some(session) = store.selected_session() else {
            return div().into_any_element();
        };
        let t = cx.theme().clone();
        let sid = session.sid.clone();
        let picked = store.agent_of(&sid).to_string();
        let tree = &session.tree;
        let reduce_motion = cx.reduce_motion();

        let rows = tree.flatten().into_iter().map(|(depth, node)| {
            let selected = node.id == picked;
            let (store, sid, id) = (self.store.clone(), sid.clone(), node.id.clone());
            h_flex()
                .id(("agent-node", hash(&node.id)))
                .gap_2()
                .pl(rems(0.75 + depth as f32 * 1.0))
                .pr_3()
                .py_1()
                .rounded(t.radius)
                .cursor_pointer()
                .when(selected, |el| el.bg(t.list_active))
                .when(!selected, |el| el.hover(|el| el.bg(t.list_hover)))
                .active(|el| el.bg(t.list_active))
                .on_click(move |_, _, cx| {
                    store.update(cx, |s, cx| {
                        s.select_agent(&sid, &id);
                        cx.notify();
                    });
                })
                .items_start()
                .child(div().pt_1().child(state_mark(node, reduce_motion, &t)))
                .child(
                    // The name gets the row's whole width; how it is doing and
                    // what it was asked share the second line.
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .font_family(t.mono_font_family.clone())
                                .child(node.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(t.muted_foreground)
                                .child(if node.description.is_empty() {
                                    state_word(node)
                                } else {
                                    format!("{} · {}", state_word(node), node.description)
                                }),
                        ),
                )
        });

        let summary = if tree.subagents() == 0 {
            "No subagents yet".to_string()
        } else {
            format!("{} running · {} in all", tree.running(), tree.subagents())
        };
        let list = v_flex()
            .id("agent-tree")
            .w(if narrow { rems(13.) } else { rems(TREE_REM) })
            .flex_none()
            .h_full()
            .border_r_1()
            .border_color(t.border)
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_0p5()
                    .child(div().text_sm().child("Agents"))
                    .child(div().text_xs().text_color(t.muted_foreground).child(summary)),
            )
            .child(
                v_flex()
                    .id("agent-rows")
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .gap_0p5()
                    .overflow_y_scrollbar()
                    .children(rows)
                    .when(tree.subagents() == 0, |el| {
                        el.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child("Subagents show here, under the agent that started them, when the model starts one."),
                        )
                    }),
            );

        let node = tree.get(&picked).or_else(|| tree.get(MAIN));
        let header = node.map(|n| {
            h_flex()
                .flex_none()
                .gap_2()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(t.border)
                .child(
                    div()
                        .text_sm()
                        .font_family(t.mono_font_family.clone())
                        .child(n.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(if n.id == MAIN {
                            "the conversation itself".to_string()
                        } else if n.description.is_empty() {
                            state_word(n)
                        } else {
                            format!("{} · {}", state_word(n), n.description)
                        }),
                )
        });

        h_flex()
            .size_full()
            .min_h_0()
            .child(list)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .children(header)
                    .child(div().flex_1().min_h_0().child(self.detail.clone())),
            )
            .into_any_element()
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn state_word(node: &AgentNode) -> String {
    match node.state {
        AgentState::Running if node.streaming => "streaming".into(),
        AgentState::Running if node.id == MAIN => "the conversation".into(),
        AgentState::Running => "running".into(),
        AgentState::Finished => node.ended_ts.map_or("finished".into(), |e| {
            format!("done in {}", fmt_ms(e.saturating_sub(node.started_ts)))
        }),
        AgentState::StoppedEarly => "stopped early".into(),
    }
}

/// The node's state at a glance: a dot that pulses while it streams, a
/// check when it finished, a warning when it stopped early.
fn state_mark(
    node: &AgentNode,
    reduce_motion: bool,
    t: &gpui_component::Theme,
) -> gpui::AnyElement {
    let slot = div()
        .size_4()
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    match node.state {
        AgentState::Finished => slot
            .child(
                Icon::new(IconName::Check)
                    .xsmall()
                    .text_color(t.muted_foreground),
            )
            .into_any_element(),
        AgentState::StoppedEarly => slot
            .child(
                Icon::new(IconName::TriangleAlert)
                    .xsmall()
                    .text_color(t.warning),
            )
            .into_any_element(),
        AgentState::Running => {
            // Green while it works (main only while it streams); it pulses
            // while a model call streams.
            let active = node.streaming || node.id != MAIN;
            let dot = div().size_2().rounded_full().bg(if active {
                t.success
            } else {
                t.muted_foreground
            });
            if node.streaming && !reduce_motion {
                slot.child(
                    dot.with_animation(
                        ("pulse", hash(&node.id)),
                        Animation::new(Duration::from_millis(1100))
                            .repeat()
                            .with_easing(pulsating_between(0.25, 1.0)),
                        |el, delta| el.opacity(delta),
                    ),
                )
                .into_any_element()
            } else {
                slot.child(dot).into_any_element()
            }
        }
    }
}
