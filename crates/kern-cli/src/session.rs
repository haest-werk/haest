//! `kern sessions`: one line per saved session.

use kern::{runtime::sessions::SessionInfo, session::iso_utc};

/// `2026-11-10 08:30Z  <id>  <cwd>  <preview>`.
pub fn format_row(s: &SessionInfo, show_cwd: bool) -> String {
    let when = iso_utc(s.modified_ms);
    let when = format!("{} {}Z", &when[..10], &when[11..16]);
    let cwd = if show_cwd {
        format!("  {}", s.cwd)
    } else {
        String::new()
    };
    format!("{when}  {}{cwd}  {}", s.id, s.preview)
}
