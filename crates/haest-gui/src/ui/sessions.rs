//! The haests with a control socket on this machine: one row each. Used in
//! the sidebar and, in a narrow window, in a sheet: one component.

use gpui::{
    App, Entity, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit_assets::IconName;
use haest::wire::Access;

use super::{access_word, dot, model_short, state_of};
use crate::{model::short, store::Store};

/// One row's facts, from discovery or from the attached session.
struct Row {
    sid: String,
    model: String,
    access: Access,
    attached: bool,
    listed: bool,
}

/// What runs after a row is picked.
type OnPick = std::rc::Rc<dyn Fn(&mut Window, &mut App)>;

/// The sessions sidebar: every haest found, the attached ones first.
#[derive(IntoElement)]
pub struct SessionList {
    store: Entity<Store>,
    /// Called after a row is picked (the sheet closes itself).
    on_pick: Option<OnPick>,
}

impl std::fmt::Debug for SessionList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionList").finish_non_exhaustive()
    }
}

impl SessionList {
    /// The list over `store`.
    pub fn new(store: Entity<Store>) -> Self {
        Self {
            store,
            on_pick: None,
        }
    }

    /// Run `f` after a row is picked.
    #[must_use]
    pub fn on_pick(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_pick = Some(std::rc::Rc::new(f));
        self
    }
}

impl RenderOnce for SessionList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let mut rows: Vec<Row> = store
            .found
            .iter()
            .flatten()
            .map(|f| Row {
                sid: f.sid.clone(),
                model: store
                    .session(&f.sid)
                    .map_or_else(|| f.model.clone(), |s| s.model.clone()),
                access: f.access,
                attached: store.is_attached(&f.sid),
                listed: true,
            })
            .collect();
        // Attached ones that left the control dir stay, as ended.
        for s in &store.sessions {
            if !rows.iter().any(|r| r.sid == s.sid) {
                rows.push(Row {
                    sid: s.sid.clone(),
                    model: s.model.clone(),
                    access: s.access,
                    attached: true,
                    listed: false,
                });
            }
        }
        let selected = store.selected.clone();
        let looked = store.found.is_some();

        let list = rows.into_iter().map(|row| {
            let is_selected = selected.as_deref() == Some(row.sid.as_str());
            let (word, color) = match store.session(&row.sid) {
                Some(_) if !row.listed => ("ended".into(), t.muted_foreground.opacity(0.6)),
                Some(s) => state_of(s, cx),
                None => (SharedString::from("not attached"), t.border),
            };
            let (pick_store, pick_sid, on_pick) =
                (self.store.clone(), row.sid.clone(), self.on_pick.clone());
            let (detach_store, detach_sid) = (self.store.clone(), row.sid.clone());
            h_flex()
                .id(SharedString::from(format!("session-{}", row.sid)))
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded(t.radius)
                .cursor_pointer()
                .when(is_selected, |el| el.bg(t.sidebar_accent))
                .when(!is_selected, |el| {
                    el.hover(|el| el.bg(t.sidebar_accent.opacity(0.5)))
                })
                .active(|el| el.bg(t.sidebar_accent))
                .on_click(move |_, window, cx| {
                    pick_store.update(cx, |s, cx| {
                        s.select(&pick_sid);
                        cx.notify();
                    });
                    if let Some(f) = &on_pick {
                        f(window, cx);
                    }
                })
                .child(
                    div()
                        .id(SharedString::from(format!("state-{}", row.sid)))
                        .child(dot(color))
                        .tooltip({
                            let word = word.clone();
                            move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(word.clone())
                                    .build(window, cx)
                            }
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .text_color(if row.listed {
                                    t.sidebar_foreground
                                } else {
                                    t.muted_foreground
                                })
                                .child(if row.model.is_empty() {
                                    "haest".to_string()
                                } else {
                                    model_short(&row.model).to_string()
                                }),
                        )
                        .child(
                            // The id keeps its width; the rest truncates before
                            // it reaches the detach button.
                            h_flex()
                                .gap_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family(t.mono_font_family.clone())
                                        .child(short(&row.sid).to_string()),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(
                                    if row.attached {
                                        format!("· {} · {}", access_word(row.access), word)
                                    } else {
                                        format!("· {}", access_word(row.access))
                                    },
                                )),
                        ),
                )
                .when(row.attached, |el| {
                    el.child(
                        Button::new(SharedString::from(format!("detach-{}", row.sid)))
                            .flex_none()
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip("Stop following this haest")
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                detach_store.update(cx, |s, cx| {
                                    s.detach(&detach_sid);
                                    cx.notify();
                                });
                            }),
                    )
                })
        });
        let list: Vec<_> = list.collect();
        let empty = list.is_empty();
        v_flex().gap_0p5().children(list).when(empty, |el| {
            el.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(if looked { "None running" } else { "Looking…" }),
            )
        })
    }
}
