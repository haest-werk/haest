//! The approval panel (I3): one queue of the approval asks of every attached
//! haest, in a small always-on-top window, answered from the keyboard.
//!
//! Safety first: the panel never takes focus by itself (an Enter typed in
//! another app must never approve anything), and an ask ignores keys for its
//! first [`ARM_MS`] ms, so a key already on its way does not answer it.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, actions, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit_assets::IconName;
use haest::wire::Answer;

use super::model_short;
use crate::{
    model::{AskId, approval_queue},
    store::Store,
};

/// How long a new ask ignores keys.
pub const ARM_MS: u64 = 400;
const CONTEXT: &str = "Approvals";

actions!(
    approvals,
    [
        /// Allow the selected ask.
        AllowAsk,
        /// Allow it, and every later one with the same tool and key.
        AlwaysAsk,
        /// Deny the selected ask.
        DenyAsk,
        /// Select the next ask.
        NextAsk,
        /// Select the previous ask.
        PrevAsk
    ]
);

/// The panel's keys: Enter allows, Cmd+Enter allows always, Delete or
/// Backspace denies, Up/Down (or k/j) pick.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", AllowAsk, Some(CONTEXT)),
        KeyBinding::new("cmd-enter", AlwaysAsk, Some(CONTEXT)),
        KeyBinding::new("backspace", DenyAsk, Some(CONTEXT)),
        KeyBinding::new("delete", DenyAsk, Some(CONTEXT)),
        KeyBinding::new("down", NextAsk, Some(CONTEXT)),
        KeyBinding::new("j", NextAsk, Some(CONTEXT)),
        KeyBinding::new("up", PrevAsk, Some(CONTEXT)),
        KeyBinding::new("k", PrevAsk, Some(CONTEXT)),
    ]);
}

type Key = (String, AskId);

/// The approval panel: every attached haest's asks in one queue, answered
/// by click or by key.
pub struct ApprovalPanel {
    store: Entity<Store>,
    focus: FocusHandle,
    selected: Option<Key>,
    /// When each ask was first shown.
    seen: HashMap<Key, Instant>,
    _observe: Subscription,
}

impl std::fmt::Debug for ApprovalPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalPanel")
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl ApprovalPanel {
    /// The panel over `store`.
    pub fn new(store: Entity<Store>, cx: &mut Context<'_, Self>) -> Self {
        let observe = cx.observe(&store, |this, _, cx| {
            this.refresh(cx);
            cx.notify();
        });
        let mut panel = Self {
            store,
            focus: cx.focus_handle(),
            selected: None,
            seen: HashMap::new(),
            _observe: observe,
        };
        panel.refresh(cx);
        panel
    }

    /// The queue's keys, oldest first.
    fn keys(&self, cx: &App) -> Vec<Key> {
        let store = self.store.read(cx);
        approval_queue(&store.sessions)
            .into_iter()
            .map(|(s, a)| (s.sid.clone(), a.id))
            .collect()
    }

    /// New asks get their arrival time; gone ones are forgotten; the
    /// selection stays on its ask, or moves to the oldest.
    fn refresh(&mut self, cx: &App) {
        let keys = self.keys(cx);
        let now = Instant::now();
        for k in &keys {
            self.seen.entry(k.clone()).or_insert(now);
        }
        self.seen.retain(|k, _| keys.contains(k));
        if self.selected.as_ref().is_none_or(|s| !keys.contains(s)) {
            self.selected = keys.first().cloned();
        }
    }

    /// Whether `key` takes keys yet.
    fn armed(&self, key: &Key) -> bool {
        self.seen
            .get(key)
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(ARM_MS))
    }

    fn answer_selected(&mut self, answer: Answer, cx: &mut Context<'_, Self>) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        if !self.armed(&key) {
            return;
        }
        self.answer(key, answer, cx);
    }

    fn answer(&mut self, (sid, ask): Key, answer: Answer, cx: &mut Context<'_, Self>) {
        self.store.update(cx, |s, cx| {
            s.answer(&sid, ask, answer);
            cx.notify();
        });
    }

    fn step(&mut self, by: isize, cx: &mut Context<'_, Self>) {
        let keys = self.keys(cx);
        if keys.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|s| keys.iter().position(|k| k == s))
            .unwrap_or(0) as isize;
        let next = (at + by).clamp(0, keys.len() as isize - 1) as usize;
        self.selected = Some(keys[next].clone());
        cx.notify();
    }
}

impl Focusable for ApprovalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ApprovalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let queue = approval_queue(&store.sessions);
        let focused = self.focus.is_focused(window);
        let count = queue.len();

        let rows: Vec<_> = queue
            .iter()
            .map(|(session, ask)| {
                let key: Key = (session.sid.clone(), ask.id);
                let selected = self.selected.as_ref() == Some(&key);
                let answerable = session.answerable(ask);
                let mut who = format!("{} · {}", model_short(&session.model), session.short_id());
                // Either side may answer it: the card may go without a click here.
                if answerable && ask.at_terminal {
                    who.push_str(" · at its terminal too");
                }
                let row_id: SharedString = format!("ask-{}-{:?}", session.sid, ask.id).into();
                let select_key = key.clone();
                let (allow_key, always_key, deny_key) = (key.clone(), key.clone(), key.clone());
                let pending = ask.answering;
                v_flex()
                    .id(row_id)
                    .gap_2()
                    .p_3()
                    .rounded(t.radius_lg)
                    .border_1()
                    .border_color(if selected && focused { t.ring } else { t.border })
                    .bg(if selected { t.list_active } else { t.group_box })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(select_key.clone());
                        cx.notify();
                    }))
                    // Line one: which tool, and which haest asks.
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .child(Icon::new(IconName::ShieldAlert).xsmall().text_color(t.warning))
                            .when(!ask.tool_name.is_empty(), |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .font_family(t.mono_font_family.clone())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(ask.tool_name.clone()),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(t.muted_foreground)
                                    .child(who),
                            ),
                    )
                    // Then why it asks, in the haest's words.
                    .child(div().text_sm().whitespace_normal().child(ask.description.clone()))
                    .when(!ask.preview.is_empty(), |el| {
                        el.child(
                            div()
                                .id(("preview", hash_key(&key)))
                                .max_h(rems(7.))
                                .overflow_y_scroll()
                                .px_2()
                                .py_1()
                                .rounded(t.radius)
                                .bg(t.background)
                                .border_1()
                                .border_color(t.border)
                                .text_xs()
                                .font_family(t.mono_font_family.clone())
                                .whitespace_normal()
                                .child(ask.preview.clone()),
                        )
                    })
                    .child(if !answerable {
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(if matches!(ask.id, AskId::Terminal(_)) {
                                "Asked on its own terminal: answer it there."
                            } else {
                                "This socket only watches: answering needs a command socket."
                            })
                            .into_any_element()
                    } else if let Some(answer) = pending {
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(format!("Sent: {}. Waiting for the haest.", answer.as_str()))
                            .into_any_element()
                    } else {
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(("allow", hash_key(&key)))
                                    .small()
                                    .primary()
                                    .label("Allow")
                                    .when(selected, |b| b.tooltip("Enter"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer(allow_key.clone(), Answer::Allow, cx)
                                    })),
                            )
                            .child(
                                Button::new(("always", hash_key(&key)))
                                    .small()
                                    .outline()
                                    .label("Always")
                                    .tooltip(match &ask.key {
                                        Some(k) => format!("Allow {} with this exact input from now on (Cmd+Enter)", if k.len() > 60 { "this call" } else { k }),
                                        None => "Allow this kind of call from now on (Cmd+Enter)".to_string(),
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer(always_key.clone(), Answer::Always, cx)
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new(("deny", hash_key(&key)))
                                    .small()
                                    .ghost()
                                    .label("Deny")
                                    .when(selected, |b| b.tooltip("Delete"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.answer(deny_key.clone(), Answer::Deny, cx)
                                    })),
                            )
                            .into_any_element()
                    })
            })
            .collect();

        v_flex()
            .id("approvals")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &AllowAsk, _, cx| this.answer_selected(Answer::Allow, cx)),
            )
            .on_action(
                cx.listener(|this, _: &AlwaysAsk, _, cx| this.answer_selected(Answer::Always, cx)),
            )
            .on_action(
                cx.listener(|this, _: &DenyAsk, _, cx| this.answer_selected(Answer::Deny, cx)),
            )
            .on_action(cx.listener(|this, _: &NextAsk, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &PrevAsk, _, cx| this.step(-1, cx)))
            .size_full()
            .bg(t.background)
            .text_color(t.foreground)
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .child(div().text_sm().child("Approvals"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(if count == 0 {
                                String::new()
                            } else {
                                format!("{count} waiting")
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(if count == 0 {
                                ""
                            } else if focused {
                                "Enter allow · Cmd+Enter always · Delete deny"
                            } else {
                                "Click to answer with the keyboard"
                            }),
                    ),
            )
            .child(if count == 0 {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::Check)
                            .small()
                            .text_color(t.muted_foreground),
                    )
                    .child(div().text_sm().child("No approvals waiting"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child("Asks from every attached haest land here."),
                    )
                    .into_any_element()
            } else {
                v_flex()
                    .id("ask-list")
                    .flex_1()
                    .min_h_0()
                    .px_3()
                    .pb_3()
                    .gap_2()
                    .overflow_y_scrollbar()
                    .children(rows)
                    .into_any_element()
            })
    }
}

fn hash_key(key: &Key) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}
