//! `haest sessions`: one line per saved session.

use haest::{core::session::iso_utc, runtime::SessionInfo};

/// `2026-11-10 08:30Z  <id>  <cwd>  <preview>`.
pub(crate) fn format_row(s: &SessionInfo, show_cwd: bool) -> String {
    let when = iso_utc(s.modified_ms);
    let when = format!("{} {}Z", &when[..10], &when[11..16]);
    let cwd = if show_cwd {
        format!("  {}", s.cwd.display())
    } else {
        String::new()
    };
    format!("{when}  {}{cwd}  {}", s.id, s.preview)
}
