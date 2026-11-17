//! /changes and /apply: reviewing what a SANDBOX conversation changed in the
//! project, and putting it on the real disk. Both are the human's commands --
//! the model can ask for them, never run them.
//!
//! The sandbox lives in this process: what is not applied when haest exits is
//! gone, so [`unapplied`] is asked before every exit.

use std::path::Path;

use haest::core::workspace::{ApplyReport, Change, ChangeKind, ChangeSet};

use crate::term::Runtime;

/// Diff text shown by /changes before it is cut; the full list is always shown.
const MAX_DIFF_CHARS: usize = 12_000;

const REAL_FS: &str = "This conversation runs on the real file system: every edit is already on disk, there is nothing to apply.";

fn line_of(c: &Change) -> String {
    let sign = match c.kind {
        ChangeKind::Added => '+',
        ChangeKind::Modified => '~',
        ChangeKind::Deleted => '-',
    };
    let conflict = if c.conflict {
        "  (CONFLICT: changed on disk since the sandbox read it)"
    } else {
        ""
    };
    format!("{sign} {}{conflict}", c.path.trim_start_matches('/'))
}

fn lines_of(cs: &[Change]) -> String {
    cs.iter().map(line_of).collect::<Vec<_>>().join("\n")
}

/// Diffs of `list`, cut at [`MAX_DIFF_CHARS`] on a character boundary.
fn cut_diffs(mut diffs: String) -> String {
    if diffs.len() <= MAX_DIFF_CHARS {
        return diffs;
    }
    let cut = (0..=MAX_DIFF_CHARS)
        .rev()
        .find(|&i| diffs.is_char_boundary(i))
        .unwrap_or(0);
    diffs.truncate(cut);
    diffs.push_str(&format!("\n... diff cut at {MAX_DIFF_CHARS} characters"));
    diffs
}

async fn diffs_of(set: &dyn ChangeSet, list: &[Change]) -> String {
    let mut diffs = String::new();
    for c in list {
        if diffs.len() > MAX_DIFF_CHARS {
            break;
        }
        diffs.push_str(&set.diff(c).await);
    }
    cut_diffs(diffs)
}

pub(crate) async fn show(rt: &Runtime) {
    let Some(set) = rt.agent().deps().shared().workspace().changes() else {
        return rt.hooks().info(REAL_FS);
    };
    let list = set.list().await;
    if list.is_empty() {
        return rt
            .hooks()
            .info("The sandbox has changed nothing in the project.");
    }
    let diffs = diffs_of(set.as_ref(), &list).await;
    rt.hooks().info(&format!(
        "{} change(s) in the sandbox, not on disk yet:\n{}\n\n{diffs}\nRun /apply to write them to the project.",
        list.len(),
        lines_of(&list)
    ));
}

/// What /apply tells the human, and whether it is a warning (something was
/// left in the sandbox).
fn apply_message(report: &ApplyReport, cwd: &Path) -> (String, bool) {
    let mut text = if report.applied.is_empty() {
        "Nothing applied.".to_string()
    } else {
        format!(
            "Applied {} change(s) to {}:\n{}",
            report.applied.len(),
            cwd.display(),
            lines_of(&report.applied)
        )
    };
    if !report.skipped.is_empty() {
        text.push_str(&format!(
            "\n\nSkipped {} -- changed on disk since the sandbox read them, so applying would overwrite your edit:\n{}",
            report.skipped.len(),
            lines_of(&report.skipped)
        ));
    }
    if !report.failed.is_empty() {
        let lines: Vec<_> = report
            .failed
            .iter()
            .map(|f| format!("{}  ({})", line_of(&f.change), f.reason))
            .collect();
        text.push_str(&format!(
            "\n\nNot applied {} -- still in the sandbox:\n{}",
            report.failed.len(),
            lines.join("\n")
        ));
    }
    let left = !report.skipped.is_empty() || !report.failed.is_empty();
    (text, left)
}

pub(crate) async fn apply(rt: &Runtime) {
    let Some(set) = rt.agent().deps().shared().workspace().changes() else {
        return rt.hooks().info(REAL_FS);
    };
    let report = set.apply().await;
    match apply_message(&report, &rt.hooks().settings().cwd) {
        (text, true) => rt.hooks().warn(&text),
        (text, false) => rt.hooks().info(&text),
    }
}

/// What exiting now would throw away, as the warning to print; None = nothing.
pub(crate) async fn unapplied(rt: &Runtime) -> Option<String> {
    let set = rt.agent().deps().shared().workspace().changes()?;
    let list = set.list().await;
    if list.is_empty() {
        return None;
    }
    Some(format!(
        "{} change(s) are only in the sandbox and are lost when haest exits:\n{}",
        list.len(),
        lines_of(&list)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_line_shows_its_kind_its_path_and_a_conflict() {
        let c = |kind, conflict| Change {
            path: "/src/a.rs".into(),
            kind,
            conflict,
        };
        assert_eq!(line_of(&c(ChangeKind::Added, false)), "+ src/a.rs");
        assert_eq!(line_of(&c(ChangeKind::Deleted, false)), "- src/a.rs");
        assert!(line_of(&c(ChangeKind::Modified, true)).starts_with("~ src/a.rs  (CONFLICT"));
    }

    #[test]
    fn apply_names_every_failed_change_and_warns_while_any_is_left() {
        use haest::core::workspace::{ApplyFailureCode, FailedChange};
        let change = |path: &str| Change {
            path: path.into(),
            kind: ChangeKind::Added,
            conflict: false,
        };
        let failed = |path: &str| FailedChange {
            change: change(path),
            code: ApplyFailureCode::Symlink,
            reason: "why".into(),
        };
        let report = ApplyReport {
            applied: vec![change("/ok.txt")],
            skipped: vec![],
            failed: vec![failed("/l"), failed("/sub")],
        };
        let (text, warn) = apply_message(&report, Path::new("/p"));
        assert!(warn);
        for path in ["ok.txt", "l", "sub"] {
            assert!(
                text.lines().any(|l| l.starts_with(&format!("+ {path}"))),
                "{path}: {text}"
            );
        }
        let all_applied = ApplyReport {
            applied: vec![change("/ok.txt")],
            ..Default::default()
        };
        assert!(!apply_message(&all_applied, Path::new("/p")).1);
    }

    #[test]
    fn a_long_diff_is_cut_on_a_character_boundary() {
        let diffs = "\u{e9}".repeat(MAX_DIFF_CHARS);
        let cut = cut_diffs(diffs);
        assert!(cut.ends_with(&format!("cut at {MAX_DIFF_CHARS} characters")));
        assert!(cut.len() < MAX_DIFF_CHARS + 60);
        assert_eq!(cut_diffs("short".into()), "short");
    }
}
