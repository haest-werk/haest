//! The turn timeline (I1): each turn a track, its model calls and tool calls
//! bars sized by duration, a batch's calls stacked, each model call shaded by
//! what the prompt cache served and wrote for it.
//!
//! The view is a window onto time, `[start, start + span)` ms from each
//! turn's start, so turns compare. Bars are placed as fractions of the plot's
//! width, so layout needs no pixel width; the gestures need one, and read it
//! from the plot's last paint.

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    AnyElement, Bounds, Context, Entity, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Render, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, canvas, div,
    prelude::FluentBuilder as _, px, relative, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit_assets::IconName;

use super::{fmt_ms, fmt_tokens, fmt_usd};
use crate::{
    model::{CacheSplit, Conn, MAIN, ModelCall, SessionModel, ToolBar, Turn},
    store::Store,
};

/// The three shares of a model call's input. Light blue: served from the
/// cache (the cheap part); yellow: written to it (a one-time premium);
/// saturated blue: plain input. Three lightnesses and two hues, so they
/// read apart at a glance and in greyscale.
fn served(t: &gpui_component::Theme) -> Hsla {
    t.chart_1
}
fn written(t: &gpui_component::Theme) -> Hsla {
    t.warning
}
fn uncached(t: &gpui_component::Theme) -> Hsla {
    t.chart_3
}

/// A tool bar narrower than its name shows the name after it, muted.
const CHAR_PX: f32 = 7.;

/// The leading label column.
const LABEL_REM: f32 = 9.;
/// One lane's height, and a bar's within it.
const LANE_REM: f32 = 1.5;
const BAR_REM: f32 = 1.125;
/// The space between tracks.
const TRACK_GAP_REM: f32 = 0.75;
/// The narrowest and widest the view zooms, ms across the plot.
const MIN_SPAN_MS: f64 = 50.;
const MAX_SPAN_MS: f64 = 6. * 60. * 60. * 1000.;

/// The Timeline tab: main's turns as tracks of model and tool calls.
pub struct TimelineView {
    store: Entity<Store>,
    /// The left edge of the view, ms from a turn's start.
    start_ms: f64,
    /// How many ms the plot shows.
    span_ms: f64,
    /// Follow the data (fit) until the reader zooms or pans.
    fitted: bool,
    /// The plot's bounds at its last paint.
    plot: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// A drag that pans: where it started, and the view's start then.
    drag: Option<(f32, f64)>,
    /// Vertical scroll of the tracks, px from the top.
    scroll_y: f32,
    /// Re-renders while a turn runs, so running bars grow.
    ticker: Option<Task<()>>,
    _observe: Subscription,
}

impl std::fmt::Debug for TimelineView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineView")
            .field("start_ms", &self.start_ms)
            .field("span_ms", &self.span_ms)
            .field("fitted", &self.fitted)
            .finish_non_exhaustive()
    }
}

impl TimelineView {
    /// The view over `store`, fitted to its turns.
    pub fn new(store: Entity<Store>, cx: &mut Context<'_, Self>) -> Self {
        let observe = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            start_ms: 0.,
            span_ms: 10_000.,
            fitted: true,
            plot: Rc::new(Cell::new(None)),
            drag: None,
            scroll_y: 0.,
            ticker: None,
            _observe: observe,
        }
    }

    /// The whole of the longest turn, with a margin.
    fn fit(&mut self, longest_ms: u64) {
        self.start_ms = 0.;
        self.span_ms = (longest_ms as f64 * 1.06).clamp(1000., MAX_SPAN_MS);
    }

    /// Zoom by `factor` (under 1 zooms in) around the pointer at `x`.
    fn zoom(&mut self, factor: f64, x: Pixels) {
        let Some(plot) = self.plot.get() else {
            return;
        };
        let frac = f64::from(((x - plot.left()) / plot.size.width).clamp(0., 1.));
        let anchor = self.start_ms + frac * self.span_ms;
        let span = (self.span_ms * factor).clamp(MIN_SPAN_MS, MAX_SPAN_MS);
        self.start_ms = (anchor - frac * span).max(-span * 0.5);
        self.span_ms = span;
        self.fitted = false;
    }

    /// Pan by `dx` px (positive moves later in time into view).
    fn pan(&mut self, dx: f32) {
        let Some(plot) = self.plot.get() else {
            return;
        };
        let width = f32::from(plot.size.width).max(1.);
        self.start_ms = (self.start_ms + f64::from(dx) / f64::from(width) * self.span_ms)
            .max(-self.span_ms * 0.5);
        self.fitted = false;
    }

    fn on_scroll(&mut self, ev: &ScrollEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        let d = ev.delta.pixel_delta(window.line_height());
        let (dx, dy) = (f32::from(d.x), f32::from(d.y));
        if ev.modifiers.platform || ev.modifiers.control {
            // Scrolling up zooms in.
            self.zoom(
                (1.0 - f64::from(dy) * 0.01).clamp(0.5, 2.0).recip(),
                ev.position.x,
            );
        } else if dx.abs() > dy.abs() || ev.modifiers.shift {
            let along = if dx.abs() > dy.abs() { dx } else { dy };
            self.pan(-along);
        } else {
            self.scroll_y = (self.scroll_y - dy).max(0.);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Keep re-rendering while a turn runs; stop when none does.
    fn tick(&mut self, running: bool, cx: &mut Context<'_, Self>) {
        if !running {
            self.ticker = None;
            return;
        }
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }
}

/// "Now" for a live session: the clock; for one that ended or is replayed:
/// its last frame.
fn now_for(s: &SessionModel, replay: bool) -> u64 {
    if replay || s.conn != Conn::Live {
        s.last_ts
    } else {
        haest::wire::now_ms()
    }
}

impl Render for TimelineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let Some(session) = store.selected_session() else {
            return div().into_any_element();
        };
        let replay = store.replay;
        let now = now_for(session, replay);
        let Some(timeline) = session.timelines.get(MAIN) else {
            return div().into_any_element();
        };
        let turns = timeline.turns.clone();
        let running = timeline.is_running() && session.conn == Conn::Live && !replay;
        let longest = timeline.longest_ms(now);
        if self.fitted {
            self.fit(longest);
        }
        self.tick(running, cx);
        let t = cx.theme().clone();

        if turns.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(div().text_sm().child("No turns yet"))
                .child(
                    div()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .max_w(rems(28.))
                        .text_center()
                        .child("Each turn becomes a track: its model calls on the first lane, its tool calls below, the calls of one batch stacked."),
                )
                .into_any_element();
        }

        let rem = f32::from(window.rem_size());
        let track_px = |turn: &Turn| (turn.lanes().max(2) as f32 * LANE_REM + TRACK_GAP_REM) * rem;
        let content_h: f32 = turns.iter().map(track_px).sum();
        let view_h = self.plot.get().map_or(400., |b| f32::from(b.size.height));
        self.scroll_y = self.scroll_y.min((content_h - view_h + rem).max(0.));

        let (start, span) = (self.start_ms, self.span_ms);
        let plot_cell = self.plot.clone();
        // Before the first paint: about what the plot gets of the window.
        let plot_w = self.plot.get().map_or_else(
            || (f32::from(window.viewport_size().width) - (LABEL_REM + 15.) * rem).max(100.),
            |b| f32::from(b.size.width),
        );

        let mut y = -self.scroll_y;
        let mut tracks: Vec<AnyElement> = Vec::new();
        for turn in &turns {
            let h = track_px(turn);
            if y + h >= 0. && y <= view_h + rem {
                tracks.push(track(turn, y, start, span, now, plot_w, &t).into_any_element());
            }
            y += h;
        }

        v_flex()
            .size_full()
            .min_h_0()
            .child(toolbar(self, &t, longest, cx))
            .child(axis(start, span, &t))
            .child(
                div()
                    .id("timeline-plot")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .cursor(if self.drag.is_some() {
                        gpui::CursorStyle::ClosedHand
                    } else {
                        gpui::CursorStyle::Arrow
                    })
                    .on_scroll(cx.listener(Self::on_scroll))
                    .on_pinch(cx.listener(|this, ev: &gpui::PinchEvent, _, cx| {
                        this.zoom((1.0 - f64::from(ev.delta)).clamp(0.5, 2.0), ev.position.x);
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.drag = Some((f32::from(ev.position.x), this.start_ms));
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        let Some((x0, start0)) = this.drag else {
                            return;
                        };
                        if ev.pressed_button != Some(MouseButton::Left) {
                            this.drag = None;
                            cx.notify();
                            return;
                        }
                        let Some(plot) = this.plot.get() else {
                            return;
                        };
                        let width = f32::from(plot.size.width).max(1.);
                        let dx = f32::from(ev.position.x) - x0;
                        this.start_ms = start0 - f64::from(dx) / f64::from(width) * this.span_ms;
                        this.fitted = false;
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.drag = None;
                            cx.notify();
                        }),
                    )
                    // Grid lines behind the tracks.
                    .child(grid(start, span, &t))
                    .children(tracks)
                    // The plot's bounds, for the gestures (the label column excluded).
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                let label = px(LABEL_REM * rem);
                                let mut plot = bounds;
                                plot.origin.x += label;
                                plot.size.width -= label;
                                plot_cell.set(Some(plot));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
            .into_any_element()
    }
}

fn toolbar(
    view: &TimelineView,
    t: &gpui_component::Theme,
    longest: u64,
    cx: &mut Context<'_, TimelineView>,
) -> impl IntoElement {
    let swatch = |color: Hsla, label: &'static str| {
        h_flex()
            .gap_1()
            .child(div().w_3().h_2().rounded_sm().bg(color))
            .child(label)
    };
    h_flex()
        .flex_none()
        .gap_4()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(t.border)
        .text_xs()
        .text_color(t.muted_foreground)
        .child(div().text_color(t.foreground).child("Model call input"))
        .child(swatch(served(t), "served from cache"))
        .child(swatch(written(t), "written to cache"))
        .child(swatch(uncached(t), "uncached"))
        .child(swatch(t.secondary, "tool call"))
        .child(div().flex_1())
        .child(format!("{} across", fmt_ms(view.span_ms.max(0.) as u64)))
        .child(
            Button::new("timeline-fit")
                .xsmall()
                .ghost()
                .icon(IconName::Maximize2)
                .label("Fit")
                .disabled(view.fitted)
                .tooltip("Show every turn whole (pinch or Cmd+scroll zooms, drag or swipe pans)")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fitted = true;
                    this.fit(longest);
                    cx.notify();
                })),
        )
}

/// A tick step that gives about 6 ticks across `span` ms.
fn tick_step(span: f64) -> f64 {
    let raw = span / 6.;
    let pow = 10f64.powf(raw.log10().floor());
    [1., 2., 5., 10.]
        .iter()
        .map(|m| m * pow)
        .find(|s| *s >= raw)
        .unwrap_or(10. * pow)
}

fn ticks(start: f64, span: f64) -> Vec<(f32, f64)> {
    let step = tick_step(span);
    let first = (start / step).ceil() * step;
    let mut out = Vec::new();
    let mut at = first;
    while at <= start + span {
        if at >= 0. {
            out.push((((at - start) / span) as f32, at));
        }
        at += step;
    }
    out
}

fn axis(start: f64, span: f64, t: &gpui_component::Theme) -> impl IntoElement {
    h_flex()
        .flex_none()
        .h(rems(1.5))
        .border_b_1()
        .border_color(t.border)
        .text_xs()
        .text_color(t.muted_foreground)
        .child(div().w(rems(LABEL_REM)).flex_none().px_4().child("Turn"))
        .child(
            div()
                .relative()
                .flex_1()
                .h_full()
                .overflow_hidden()
                .children(
                    ticks(start, span)
                        .into_iter()
                        .filter(|(frac, _)| *frac < 0.95)
                        .map(|(frac, ms)| {
                            div()
                                .absolute()
                                .left(relative(frac))
                                .top_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .pl_1()
                                .border_l_1()
                                .border_color(t.border)
                                .child(fmt_ms(ms as u64))
                        }),
                ),
        )
}

fn grid(start: f64, span: f64, t: &gpui_component::Theme) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(rems(LABEL_REM))
        .right_0()
        .children(ticks(start, span).into_iter().map(|(frac, _)| {
            div()
                .absolute()
                .left(relative(frac))
                .top_0()
                .bottom_0()
                .w(px(1.))
                .bg(t.border.opacity(0.5))
        }))
}

/// One turn's track at `y` px.
fn track(
    turn: &Turn,
    y: f32,
    start: f64,
    span: f64,
    now: u64,
    plot_w: f32,
    t: &gpui_component::Theme,
) -> impl IntoElement {
    let end = turn.end_or(now);
    let took = end - turn.start_ts;
    let mut meta = vec![fmt_ms(took)];
    if let Some(usd) = turn.usage.as_ref().and_then(|u| u.cost_usd) {
        meta.push(fmt_usd(usd));
    }
    // Line one says how it ended when that was not the model finishing.
    let ending = if turn.end_ts.is_none() {
        Some("running".to_string())
    } else {
        turn.stop_reason
            .as_deref()
            .filter(|r| *r != "end_turn")
            .map(|r| r.replace('_', " "))
    };
    // Two lanes at least: the label's two lines fit its own track.
    let lanes = turn.lanes().max(2);
    let frac = |ts: u64| ((ts.saturating_sub(turn.start_ts) as f64 - start) / span) as f32;
    let width = |a: u64, b: u64| (((b.saturating_sub(a)) as f64 / span) as f32).max(0.);

    let mut bars: Vec<AnyElement> = Vec::new();
    for (i, call) in turn.calls.iter().enumerate() {
        let b = call.end_ts.unwrap_or(now);
        bars.push(
            model_bar(
                turn.number,
                i,
                call,
                frac(call.start_ts),
                width(call.start_ts, b),
                now,
                t,
            )
            .into_any_element(),
        );
    }
    for bar in &turn.tools {
        let b = bar.end_ts.unwrap_or(now);
        bars.push(
            tool_bar(
                turn.number,
                bar,
                frac(bar.start_ts),
                width(bar.start_ts, b),
                plot_w,
                t,
            )
            .into_any_element(),
        );
    }

    h_flex()
        .absolute()
        .top(px(y))
        .left_0()
        .right_0()
        .h(rems(lanes as f32 * LANE_REM))
        .items_start()
        .child(
            v_flex()
                .w(rems(LABEL_REM))
                .flex_none()
                .px_4()
                .child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .child(div().flex_none().child(format!("Turn {}", turn.number)))
                        .when_some(ending, |el, e| {
                            el.child(div().min_w_0().truncate().text_color(t.muted_foreground).child(e))
                        })
                        .when(turn.partial, |el| {
                            el.child(
                                div()
                                    .id(("partial", turn.number))
                                    .text_color(t.warning)
                                    .child("partial")
                                    .tooltip(|window, cx| {
                                        Tooltip::new("Frames of this turn were lost: some of its bars are missing.")
                                            .build(window, cx)
                                    }),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .truncate()
                        .text_color(t.muted_foreground)
                        .child(meta.join(" · ")),
                ),
        )
        .child(
            div()
                .relative()
                .flex_1()
                .h_full()
                .overflow_hidden()
                .children(bars),
        )
}

fn lane_top(lane: usize) -> gpui::Rems {
    rems(lane as f32 * LANE_REM + (LANE_REM - BAR_REM) / 2.)
}

fn model_bar(
    turn: usize,
    i: usize,
    call: &ModelCall,
    left: f32,
    width: f32,
    now: u64,
    t: &gpui_component::Theme,
) -> impl IntoElement {
    let split = call.usage.as_ref().and_then(CacheSplit::of);
    let took = call.end_ts.unwrap_or(now).saturating_sub(call.start_ts);
    let mut tip = vec![format!("Model call · {}", fmt_ms(took)), call.model.clone()];
    if let Some(u) = &call.usage {
        let input = u.input + u.cache_read + u.cache_write;
        let mut line = format!("{} in · {} out", fmt_tokens(input), fmt_tokens(u.output));
        if let Some(usd) = u.cost_usd {
            line.push_str(&format!(" · {}", fmt_usd(usd)));
        }
        tip.push(line);
        if let Some(s) = split {
            tip.push(format!(
                "cache: {:.0}% served · {:.0}% written · {:.0}% uncached",
                s.served * 100.,
                s.written * 100.,
                s.uncached * 100.
            ));
        }
    } else if call.end_ts.is_some() {
        tip.push("no usage reported: the call ended early".into());
    } else {
        tip.push("streaming".into());
    }
    if call.error {
        tip.push("failed".into());
    }
    let tip: SharedString = tip.join("\n").into();
    let bar = div()
        .id(("model-bar", (turn * 1000 + i) as u64))
        .absolute()
        .left(relative(left))
        .w(relative(width))
        .min_w(px(2.))
        .top(lane_top(0))
        .h(rems(BAR_REM))
        .rounded_sm()
        .overflow_hidden()
        .flex()
        .hover(|el| el.opacity(0.85))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    let bar = match split {
        Some(s) => bar
            .child(div().h_full().w(relative(s.served)).bg(served(t)))
            .child(div().h_full().w(relative(s.written)).bg(written(t)))
            .child(div().h_full().w(relative(s.uncached)).bg(uncached(t))),
        // Still streaming, or no usage reported: one faint fill.
        None => bar.bg(uncached(t).opacity(0.45)),
    };
    bar.when(call.error, |el| el.border_1().border_color(t.danger))
}

fn tool_bar(
    turn: usize,
    bar: &ToolBar,
    left: f32,
    width: f32,
    plot_w: f32,
    t: &gpui_component::Theme,
) -> impl IntoElement {
    let fits = width * plot_w >= bar.name.chars().count() as f32 * CHAR_PX + 10.;
    let took = bar.end_ts.map(|e| e.saturating_sub(bar.start_ts));
    let tip: SharedString = format!(
        "{} · {}{}\nbatch {}, call {}",
        bar.name,
        took.map_or("running".to_string(), fmt_ms),
        if bar.error { " · failed" } else { "" },
        bar.batch + 1,
        bar.lane + 1,
    )
    .into();
    let id = SharedString::from(format!("tool-bar-{turn}-{}", bar.call_id));
    div()
        .id(id)
        .absolute()
        .left(relative(left))
        .w(relative(width))
        .min_w(px(3.))
        .top(lane_top(bar.lane + 1))
        .h(rems(BAR_REM))
        .rounded_sm()
        .bg(t.secondary)
        .when(bar.end_ts.is_none(), |el| el.opacity(0.7))
        .when(bar.error, |el| el.border_l_2().border_color(t.danger))
        .text_xs()
        .whitespace_nowrap()
        .hover(|el| el.bg(t.secondary_hover))
        .map(|el| {
            if fits {
                el.flex()
                    .items_center()
                    .px_1()
                    .overflow_hidden()
                    .text_color(t.foreground)
                    .child(bar.name.clone())
            } else {
                el.child(
                    div()
                        .absolute()
                        .left(relative(1.))
                        .top_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .pl_1()
                        .text_color(t.muted_foreground)
                        .child(bar.name.clone()),
                )
            }
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_land_on_round_steps_inside_the_view() {
        assert_eq!(tick_step(6000.), 1000.);
        assert_eq!(tick_step(9000.), 2000.);
        let t = ticks(500., 6000.);
        assert_eq!(t.first().map(|t| t.1), Some(1000.));
        assert!(t.iter().all(|(f, _)| (0. ..=1.).contains(f)));
    }
}
