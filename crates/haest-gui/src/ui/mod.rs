//! The views. Each reads the [`Store`](crate::store::Store) and nothing else.

pub mod agents;
pub mod approvals;
pub mod sessions;
pub mod stream;
pub mod timeline;
pub mod window;

use gpui::{App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode, h_flex};
use haest::wire::{Access, State};

use crate::model::{Conn, SessionModel};

/// The base font size: `rem`. Desktop tool density.
pub const BASE_FONT: f32 = 14.;

/// The theme: gpui-component's dark theme at desktop density.
pub fn init_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |t| {
        t.font_size = px(BASE_FONT);
        t.mono_font_size = px(BASE_FONT - 1.);
    });
}

/// What a session is doing, as one word and the color that means it.
pub fn state_of(s: &SessionModel, cx: &App) -> (SharedString, Hsla) {
    let t = cx.theme();
    match &s.conn {
        Conn::Connecting => ("connecting".into(), t.muted_foreground),
        Conn::Lost(_) => ("disconnected".into(), t.danger),
        Conn::Live => match s.state {
            State::Running => ("running".into(), t.success),
            _ if s.queued > 0 => ("input queued".into(), t.warning),
            _ => ("idle".into(), t.muted_foreground),
        },
    }
}

/// A state dot: 8 px, the state's color.
pub fn dot(color: Hsla) -> impl IntoElement {
    div().size_2().flex_none().rounded_full().bg(color)
}

/// The access as people read it: `command` or `watch`.
pub fn access_word(a: Access) -> &'static str {
    match a {
        Access::Command => "command",
        _ => "watch",
    }
}

/// `820 ms`, `4.2 s`, `3 m 12 s`.
pub fn fmt_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.)
    } else {
        format!("{} m {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// `950`, `12.3k`, `1.2M`.
pub fn fmt_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.)
    }
}

/// `$0.0123` under a dollar (a cheap model's turn costs fractions of a cent),
/// `$1.50` above.
pub fn fmt_usd(usd: f64) -> String {
    if usd < 1. {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

/// The model's short name: `claude-haiku-5-5` for `anthropic:claude-haiku-5-5`,
/// `claude-opus-5.5` for `openrouter:anthropic/claude-opus-5.5`.
pub fn model_short(spec: &str) -> &str {
    let after_colon = spec.rsplit_once(':').map_or(spec, |(_, m)| m);
    after_colon.rsplit_once('/').map_or(after_colon, |(_, m)| m)
}

/// A label and a value in one muted line: `model` `haiku`.
pub fn meta(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .child(label.into()),
        )
        .child(div().text_color(cx.theme().foreground).child(value.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_tokens_and_models_read_short() {
        assert_eq!(fmt_ms(820), "820 ms");
        assert_eq!(fmt_ms(4210), "4.2 s");
        assert_eq!(fmt_ms(192_000), "3 m 12 s");
        assert_eq!(fmt_tokens(12_300), "12.3k");
        assert_eq!(
            model_short("anthropic:claude-haiku-5-5"),
            "claude-haiku-5-5"
        );
        assert_eq!(
            model_short("openrouter:anthropic/claude-opus-5.5"),
            "claude-opus-5.5"
        );
        assert_eq!(fmt_usd(0.0123), "$0.0123");
        assert_eq!(fmt_usd(1.5), "$1.50");
    }
}
