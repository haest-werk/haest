//! The caps follow the window.

use super::*;

fn bare() -> Settings {
    Settings::bare("/w", "/h")
}

#[test]
fn token_caps_follow_the_window_unless_set() {
    let mut s = bare();
    let l = s.turn_caps_for(200_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (180_000, 2_000_000)
    );
    assert_eq!(l.max_turn_time, std::time::Duration::from_secs(3600));
    s.caps.max_request_tokens = Some(0);
    assert_eq!(s.turn_caps_for(200_000).max_request_tokens, 0);
    assert_eq!(
        (s.window(None), s.window(Some(1_000_000))),
        (200_000, 1_000_000)
    );
    s.caps.context_window = Some(50_000);
    assert_eq!(s.window(Some(1_000_000)), 50_000);
}

/// `context_window` takes any u64: a huge one saturates the caps that follow
/// it, and a window that is not a multiple of 10 keeps its exact 90%.
#[test]
fn token_caps_on_a_huge_window_saturate_instead_of_overflowing() {
    let s = bare();
    let l = s.turn_caps_for(u64::MAX);
    assert_eq!(l.max_turn_tokens, u64::MAX);
    assert_eq!(l.max_request_tokens, (u128::from(u64::MAX) * 9 / 10) as u64);
    let l = s.turn_caps_for(2_000_000_000_000_000_000);
    assert_eq!(
        (l.max_request_tokens, l.max_turn_tokens),
        (1_800_000_000_000_000_000, u64::MAX)
    );
    assert_eq!(s.turn_caps_for(1_048_576).max_request_tokens, 943_718);
}
