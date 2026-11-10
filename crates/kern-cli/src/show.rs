//! `kern settings`: every setting, its value and where it came from, under
//! the files kern read (or skipped) to get there.

use kern::settings::{Loaded, ProjectFile, Source};

const NONE: &str = "-";

/// The report, one line each.
pub fn settings_report(loaded: &Loaded) -> Vec<String> {
    let r = &loaded.resolved;
    let s = &loaded.settings;
    let user = s.config_dir.join("config.toml");
    let mut out = vec![format!("config file:  {}", user.display())];
    out.push(match &r.project_file {
        ProjectFile::None => format!("project file: {NONE}"),
        ProjectFile::Read(p) => format!("project file: {}", p.display()),
        ProjectFile::Untrusted(p) => format!(
            "project file: {} (skipped: not in trusted_dirs)",
            p.display()
        ),
    });
    let trusted = if s.trusted { "trusted" } else { "not trusted" };
    out.push(format!("this dir:     {} ({trusted})", s.cwd.display()));
    out.push(String::new());
    for (def, value, source) in r.rows() {
        let value = match value {
            None => NONE,
            Some("") => "none",
            Some(v) => v,
        };
        let from = source.map_or(NONE.to_string(), Source::to_string);
        out.push(format!("{:<24} {:<40} {from}", def.key, value));
    }
    out.extend(s.notes.iter().map(|n| format!("\nnote: {n}")));
    out
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use kern::settings::{Inputs, settle};

    use super::*;

    #[test]
    fn each_row_names_its_value_and_source_and_an_unset_one_says_so() {
        let flags = vec![("tools".to_string(), "read".to_string())];
        let loaded = settle(&Inputs {
            cwd: Path::new("/w"),
            home: Path::new("/h"),
            env: &HashMap::new(),
            flags: &flags,
            user_file: None,
            project_file: None,
        })
        .unwrap();
        let report = settings_report(&loaded);
        let row = |key: &str| {
            report
                .iter()
                .find(|l| l.split_whitespace().next() == Some(key))
                .unwrap()
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(row("tools"), ["tools", "read", "--tools"]);
        assert_eq!(row("context"), ["context", "none", "default"]);
        assert_eq!(row("effort"), ["effort", "-", "-"]);
    }
}
