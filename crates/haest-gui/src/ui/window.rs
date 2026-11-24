//! The main window: the sessions on the leading edge, the attached one on
//! the rest, its three views in tabs, and (on a command socket) its
//! controls and the composer.

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, actions, div, prelude::FluentBuilder as _, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IndexPath, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    searchable_list::{SearchableGroup, SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit_assets::IconName;
use haest::wire::State;

use super::{
    access_word,
    agents::AgentsView,
    approvals::ApprovalPanel,
    dot, fmt_usd, model_short,
    sessions::SessionList,
    state_of,
    stream::{Follow, StreamPanel},
    timeline::TimelineView,
};
use crate::{
    model::{Conn, approval_queue},
    store::{Store, Tab as View},
};

/// Under this width the sidebar folds into a sheet.
const NARROW_PX: f32 = 760.;
const SIDEBAR_REM: f32 = 15.;
const CONTEXT: &str = "HaestGui";

actions!(
    haest_gui,
    [
        /// Stop the running turn.
        Interrupt,
        /// Show the approval panel.
        ShowApprovals
    ]
);

/// The main window's keys: Cmd+. interrupts, Cmd+Shift+A shows the panel.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-.", Interrupt, Some(CONTEXT)),
        KeyBinding::new("cmd-shift-a", ShowApprovals, Some(CONTEXT)),
    ]);
}

/// A model in the picker: haest's catalog row.
#[derive(Debug, Clone)]
pub struct ModelItem {
    spec: SharedString,
    label: SharedString,
}

impl SearchableListItem for ModelItem {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.label.clone()
    }
    fn value(&self) -> &SharedString {
        &self.spec
    }
    fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.label.to_lowercase().contains(&q) || self.spec.to_lowercase().contains(&q)
    }
}

type ModelPicker = SelectState<SearchableVec<SearchableGroup<ModelItem>>>;
type EffortPicker = SelectState<Vec<SharedString>>;

/// haest's own catalog, as its groups.
fn model_groups() -> Vec<SearchableGroup<ModelItem>> {
    haest::providers::Catalog::builtin()
        .groups()
        .iter()
        .map(|g| {
            SearchableGroup::new(g.label.clone()).items(g.models.iter().map(|m| ModelItem {
                spec: m.spec.clone().into(),
                label: m.label.clone().into(),
            }))
        })
        .collect()
}

const EFFORTS: [&str; 7] = ["default", "off", "low", "medium", "high", "xhigh", "max"];

/// How the app opens the approval panel (the bootstrap owns the window).
pub type OpenApprovals = std::rc::Rc<dyn Fn(bool, &mut App)>;

/// The main window: the sessions, and the shown one's Stream, Timeline and
/// Agents tabs.
pub struct MainWindow {
    store: Entity<Store>,
    stream: Entity<StreamPanel>,
    timeline: Entity<TimelineView>,
    agents: Entity<AgentsView>,
    composer: Entity<TextareaState>,
    model: Entity<ModelPicker>,
    effort: Entity<EffortPicker>,
    open_approvals: OpenApprovals,
    /// The session and values the pickers last showed.
    shown: Option<(String, String, Option<String>)>,
    running: bool,
    _subs: Vec<Subscription>,
}

impl std::fmt::Debug for MainWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainWindow")
            .field("shown", &self.shown)
            .field("running", &self.running)
            .finish_non_exhaustive()
    }
}

impl MainWindow {
    /// The window over `store`; `open_approvals` shows the approval panel.
    pub fn new(
        store: Entity<Store>,
        open_approvals: OpenApprovals,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let stream = cx.new(|cx| StreamPanel::new(store.clone(), Follow::Main, cx));
        let timeline = cx.new(|cx| TimelineView::new(store.clone(), cx));
        let agents = cx.new(|cx| AgentsView::new(store.clone(), cx));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Message this haest")
        });
        let model = cx.new(|cx| {
            SelectState::new(SearchableVec::new(model_groups()), None, window, cx).searchable(true)
        });
        let effort = cx.new(|cx| {
            let items: Vec<SharedString> = EFFORTS.iter().map(|e| SharedString::from(*e)).collect();
            SelectState::new(items, Some(IndexPath::default()), window, cx)
        });
        let subs = vec![
            cx.observe_in(&store, window, |this, _, window, cx| this.sync(window, cx)),
            cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send(window, cx);
                }
            }),
            cx.subscribe(
                &model,
                |this, _, ev: &SelectEvent<SearchableVec<SearchableGroup<ModelItem>>>, cx| {
                    let SelectEvent::Confirm(Some(spec)) = ev else {
                        return;
                    };
                    let spec = spec.to_string();
                    if this.shown.as_ref().is_some_and(|s| s.1 == spec) {
                        return;
                    }
                    this.store.update(cx, |s, cx| {
                        s.set_model(spec);
                        cx.notify();
                    });
                },
            ),
            cx.subscribe(
                &effort,
                |this, _, ev: &SelectEvent<Vec<SharedString>>, cx| {
                    let SelectEvent::Confirm(Some(word)) = ev else {
                        return;
                    };
                    let effort = (word.as_ref() != "default").then(|| word.to_string());
                    if this.shown.as_ref().is_some_and(|s| s.2 == effort) {
                        return;
                    }
                    this.store.update(cx, |s, cx| {
                        s.set_effort(effort);
                        cx.notify();
                    });
                },
            ),
        ];
        let mut this = Self {
            store,
            stream,
            timeline,
            agents,
            composer,
            model,
            effort,
            open_approvals,
            shown: None,
            running: false,
            _subs: subs,
        };
        this.sync(window, cx);
        this
    }

    /// Keep the pickers and the composer's hint on the shown session.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let store = self.store.read(cx);
        let Some(s) = store.selected_session() else {
            self.shown = None;
            cx.notify();
            return;
        };
        let now = (s.sid.clone(), s.model.clone(), s.effort.clone());
        let running = s.state == State::Running;
        if self.shown.as_ref() != Some(&now) {
            let (model, effort) = (now.1.clone(), now.2.clone());
            self.model.update(cx, |m, cx| {
                m.set_selected_value(&SharedString::from(model), window, cx);
            });
            self.effort.update(cx, |e, cx| {
                let word = SharedString::from(effort.unwrap_or_else(|| "default".into()));
                e.set_selected_value(&word, window, cx);
            });
            self.shown = Some(now);
        }
        if running != self.running {
            self.running = running;
            let hint = if running {
                "Add to the running turn: it joins at its next step"
            } else {
                "Message this haest"
            };
            self.composer
                .update(cx, |c, cx| c.set_placeholder(hint, window, cx));
        }
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let sent = self.store.update(cx, |s, cx| {
            let sent = s.send_turn(text);
            cx.notify();
            sent
        });
        if sent {
            self.composer
                .update(cx, |c, cx| c.set_value("", window, cx));
        }
    }

    fn interrupt(&mut self, _: &Interrupt, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.store.update(cx, |s, cx| {
            s.interrupt();
            cx.notify();
        });
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let narrow = f32::from(window.viewport_size().width) < NARROW_PX;
        let waiting = approval_queue(&self.store.read(cx).sessions).len();
        let open_approvals = self.open_approvals.clone();

        let title_bar = TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("haest-gui"),
                )
                .child(div().flex_1())
                .child(
                    Button::new("show-approvals")
                        .xsmall()
                        .when(waiting > 0, |b| b.warning())
                        .when(waiting == 0, |b| b.ghost())
                        .icon(IconName::ShieldAlert)
                        .label(if waiting == 0 {
                            "Approvals".to_string()
                        } else {
                            format!("{waiting} waiting")
                        })
                        .tooltip("Show the approval panel (Cmd+Shift+A)")
                        .on_click(move |_, _, cx| open_approvals(true, cx)),
                ),
        );

        let sidebar = (!narrow).then(|| {
            v_flex()
                .w(rems(SIDEBAR_REM))
                .flex_none()
                .h_full()
                .bg(t.sidebar)
                .border_r_1()
                .border_color(t.sidebar_border)
                .child(
                    div()
                        .px_4()
                        .pt_3()
                        .pb_2()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child("Sessions"),
                )
                .child(
                    div()
                        .id("session-list")
                        .flex_1()
                        .min_h_0()
                        .px_1()
                        .overflow_y_scroll()
                        .child(SessionList::new(self.store.clone())),
                )
        });

        let open_approvals = self.open_approvals.clone();
        v_flex()
            .id("haest-gui")
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::interrupt))
            .on_action(move |_: &ShowApprovals, _, cx| open_approvals(true, cx))
            .size_full()
            .bg(t.background)
            .text_color(t.foreground)
            .child(title_bar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .children(sidebar)
                    .child(self.main(narrow, cx)),
            )
    }
}

impl MainWindow {
    fn main(&mut self, narrow: bool, cx: &mut Context<'_, Self>) -> AnyElement {
        let t = cx.theme().clone();
        let store = self.store.read(cx);
        let Some(s) = store.selected_session() else {
            return v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .when(narrow, |el| el.child(self.sessions_button_row(cx)))
                .child(no_session(store, cx))
                .into_any_element();
        };
        let (state_word, state_color) = state_of(s, cx);
        let mut meta = vec![s.short_id().to_string(), access_word(s.access).to_string()];
        if let Some(usd) = s.usd.filter(|u| *u > 0.) {
            meta.push(fmt_usd(usd));
        }
        if s.messages > 0 {
            meta.push(format!("{} messages", s.messages));
        }
        let commandable = s.can_command();
        let running = s.state == State::Running && s.conn == Conn::Live;
        let tab = store.tab;
        let agents_running = s.tree.running();
        let queued_idle = s.queued > 0 && !running;
        let title = if s.model.is_empty() {
            "haest".to_string()
        } else {
            model_short(&s.model).to_string()
        };

        let header = h_flex()
            .flex_none()
            .gap_3()
            .px_4()
            .py_2()
            .when(narrow, |el| el.child(self.sessions_button(cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_base().truncate().child(title))
                            .child(dot(state_color))
                            .child(div().text_xs().text_color(state_color).child(state_word)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(t.muted_foreground)
                            .child(meta.join(" · ")),
                    ),
            )
            .when(commandable, |el| {
                el.child(
                    div()
                        .w(if narrow { rems(9.) } else { rems(14.) })
                        .min_w_0()
                        .child(
                            Select::new(&self.model)
                                .small()
                                .search_placeholder("Find a model")
                                .placeholder("Model")
                                .menu_width(rems(20.))
                                .accessibility_label("Model, from the next turn on"),
                        ),
                )
                .child(
                    div().w(rems(7.)).flex_none().child(
                        Select::new(&self.effort)
                            .small()
                            .accessibility_label("Reasoning effort, from the next turn on"),
                    ),
                )
                .child(
                    Button::new("interrupt")
                        .small()
                        .outline()
                        .icon(IconName::CircleStop)
                        .label("Interrupt")
                        .disabled(!running)
                        .tooltip(if running {
                            "Stop the running turn (Cmd+.)"
                        } else {
                            "No turn is running"
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.interrupt(&Interrupt, window, cx)
                        })),
                )
            });

        let store_for_tabs = self.store.clone();
        let tabs = TabBar::new("views")
            .underline()
            .small()
            .px_2()
            .selected_index(match tab {
                View::Stream => 0,
                View::Timeline => 1,
                View::Agents => 2,
            })
            .on_click(move |ix, _, cx| {
                store_for_tabs.update(cx, |s, cx| {
                    s.tab = match ix {
                        1 => View::Timeline,
                        2 => View::Agents,
                        _ => View::Stream,
                    };
                    cx.notify();
                });
            })
            .child(Tab::new().label("Stream"))
            .child(Tab::new().label("Timeline"))
            .child(Tab::new().label(if agents_running > 0 {
                format!("Agents ({agents_running} running)")
            } else {
                "Agents".to_string()
            }));

        let body: AnyElement = match tab {
            View::Stream => self.stream.clone().into_any_element(),
            View::Timeline => self.timeline.clone().into_any_element(),
            View::Agents => self.agents.clone().into_any_element(),
        };

        let composer = (commandable && tab == View::Stream).then(|| {
            let empty = self.composer.read(cx).value().trim().is_empty();
            v_flex()
                .flex_none()
                .gap_1()
                .px_4()
                .pt_2()
                .pb_3()
                .border_t_1()
                .border_color(t.border)
                .child(
                    h_flex()
                        .gap_2()
                        .items_end()
                        .child(div().flex_1().min_w_0().child(Textarea::new(&self.composer)))
                        .child(
                            Button::new("send")
                                .primary()
                                .icon(IconName::ArrowUp)
                                .tooltip(if running { "Add to the running turn (Enter)" } else { "Send (Enter)" })
                                .disabled(empty)
                                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(if queued_idle { t.warning } else { t.muted_foreground })
                        .child(if queued_idle {
                            "Its input waits: a haest at its own prompt runs it when someone presses Enter there."
                        } else if running {
                            "Enter sends · Shift+Enter new line"
                        } else {
                            "Enter sends · Shift+Enter new line · model and effort apply from the next turn"
                        }),
                )
        });

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .flex_none()
                    .border_b_1()
                    .border_color(t.border)
                    .child(tabs),
            )
            .child(div().flex_1().min_h_0().child(body))
            .children(composer)
            .into_any_element()
    }

    fn sessions_button(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let store = self.store.clone();
        Button::new("sessions")
            .small()
            .ghost()
            .icon(IconName::PanelLeft)
            .tooltip("Sessions")
            .on_click(cx.listener(move |_, _, window, cx| {
                let store = store.clone();
                window.open_sheet_at(gpui_component::Placement::Left, cx, move |sheet, _, _| {
                    sheet.title("Sessions").size(rems(18.)).child(
                        SessionList::new(store.clone())
                            .on_pick(|window, cx| window.close_sheet(cx)),
                    )
                });
            }))
    }

    fn sessions_button_row(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        h_flex().px_4().py_2().child(self.sessions_button(cx))
    }
}

/// No session is shown: say why, and what to do.
fn no_session(store: &Store, cx: &App) -> impl IntoElement {
    let t = cx.theme();
    let found = store.found.as_ref().map_or(0, Vec::len);
    let (title, body): (&str, Vec<String>) = match &store.found {
        None => (
            "Looking for haests",
            vec![format!("Reading {}", store.dir.display())],
        ),
        Some(_) if found > 0 => (
            "Pick a haest to attach to it",
            vec![
                "Its stream, timeline and agents show here; its approval asks join the panel."
                    .into(),
            ],
        ),
        Some(_) => (
            "No haest is running with a control socket",
            vec![
                "Start one with its control socket on:".into(),
                "haest --control-socket command".into(),
                format!("Watching {}", store.dir.display()),
            ],
        ),
    };
    let mono = t.mono_font_family.clone();
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            Icon::new(IconName::SquareTerminal)
                .large()
                .text_color(t.muted_foreground),
        )
        .child(div().text_base().child(title))
        .children(body.into_iter().map(move |line| {
            let is_cmd = line.starts_with("haest ");
            div()
                .text_sm()
                .text_color(if is_cmd {
                    t.foreground
                } else {
                    t.muted_foreground
                })
                .when(is_cmd, |el| {
                    el.font_family(mono.clone())
                        .px_2()
                        .py_1()
                        .rounded(t.radius)
                        .bg(t.secondary)
                })
                .child(line)
        }))
}

/// The panel's root view, opened by the bootstrap.
pub fn approval_panel(store: Entity<Store>, cx: &mut App) -> Entity<ApprovalPanel> {
    cx.new(|cx| ApprovalPanel::new(store, cx))
}
