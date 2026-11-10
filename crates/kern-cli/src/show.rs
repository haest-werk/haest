//! `kern settings`: every setting, its value and where it came from, under
//! the files kern read (or skipped) to get there.

use kern::config::{DEFS, Loaded, ProjectFile, Source, get};

const NONE: &str = "-";

/// The report, one line each.
pub fn settings_report(loaded: &Loaded) -> Vec<String> {
    let r = &loaded.resolved;
    let s = &loaded.config.settings;
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
    let trusted = if r.trusted { "trusted" } else { "not trusted" };
    out.push(format!("this dir:     {} ({trusted})", s.cwd.display()));
    out.push(String::new());
    for def in DEFS {
        let value = get(&loaded.config, def.key);
        let source = r.source(def.key);
        let (value, from) = match value.as_deref() {
            None if source == Source::Default => (NONE.to_string(), NONE.to_string()),
            None => (NONE.to_string(), source.to_string()),
            Some("") => ("none".to_string(), source.to_string()),
            Some(v) => (v.to_string(), source.to_string()),
        };
        out.push(format!("{:<24} {:<40} {from}", def.key, value));
    }
    out.extend(loaded.notes.iter().map(|n| format!("\nnote: {n}")));
    out
}

#[cfg(test)]
mod tests {
    use kern::{config::Loader, fs::MemFs, settings::Settings};

    use super::*;

    #[tokio::test]
    async fn each_row_names_its_value_and_source_and_an_unset_one_says_so() {
        let flags = vec![("tools".to_string(), "read".to_string())];
        let loaded = Loader::new(Settings::bare("/w", "/h"))
            .flags(&flags)
            .load(&MemFs::new())
            .await
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
        assert_eq!(
            row("env_passthrough"),
            ["env_passthrough", "core", "default"]
        );
        assert_eq!(row("max_turn_usd"), ["max_turn_usd", "2", "default"]);
    }
}
