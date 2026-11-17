//! `haest settings`: every setting, its value and where it came from, under
//! the files haest read (or skipped) to get there.

use haest::config::{DEFS, Def, Kind, Loaded, ProjectFile, Source};

const NONE: &str = "-";

/// The report, one line each.
pub(crate) fn settings_report(loaded: &Loaded) -> Vec<String> {
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
        for (value, from) in rows(loaded, def) {
            out.push(format!("{:<24} {:<40} {from}", def.key, value));
        }
    }
    out.extend(loaded.notes.iter().map(|n| format!("\nnote: {n}")));
    out
}

/// A setting's value and where it came from. A setting of variables (`env`)
/// is a row per variable: its name, never its value.
fn rows(loaded: &Loaded, def: &Def) -> Vec<(String, String)> {
    let r = &loaded.resolved;
    let value = loaded.config.get(def.key);
    if let (Kind::Vars, Some(names)) = (def.kind, &value) {
        let from = |name: &str| r.var_source(def.key, name).to_string();
        return names.split(',').map(|n| (n.to_string(), from(n))).collect();
    }
    let source = r.source(def.key);
    let row = match value.as_deref() {
        None if source == Source::Default => (NONE.to_string(), NONE.to_string()),
        None => (NONE.to_string(), source.to_string()),
        Some("") => ("none".to_string(), source.to_string()),
        Some(v) => (shown(def, v), source.to_string()),
    };
    vec![row]
}

/// A value as the report shows it. The broker's URL goes without its
/// userinfo, query values and fragment, as the "Broker link" line says it:
/// the URL is not where a secret goes, but one may be there.
fn shown(def: &Def, value: &str) -> String {
    if def.key == "broker_url" {
        haest::http::__private::redacted_url(value).into_owned()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use haest::{config::Loader, core::fs::MemFs, settings::Settings};

    use super::*;

    /// The report for bare settings in `/w` with these flags.
    async fn report(flags: &[(&str, &str)]) -> Vec<String> {
        let flags: Vec<(String, String)> = flags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let loaded = Loader::new(Settings::bare("/w", "/h"))
            .flags(flags)
            .load(&MemFs::new())
            .await
            .unwrap();
        settings_report(&loaded)
    }

    #[tokio::test]
    async fn each_row_names_its_value_and_source_and_an_unset_one_says_so() {
        let report = report(&[("tools", "read")]).await;
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
        assert_eq!(row("env"), ["env", "-", "-"]);
    }

    #[tokio::test]
    async fn each_variable_is_a_row_of_its_name_and_source_and_no_value_is_shown() {
        let report = report(&[("env", "GH_TOKEN=s3cret"), ("env", "A=also-s3cret")]).await;
        let rows: Vec<Vec<&str>> = report
            .iter()
            .filter(|l| l.starts_with("env "))
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert_eq!(rows, [["env", "A", "--env"], ["env", "GH_TOKEN", "--env"]]);
        assert!(!report.join("\n").contains("s3cret"), "{report:#?}");
    }

    /// The report is what people paste when they ask for help: a broker
    /// URL shows where it goes, never a credential riding in it.
    #[tokio::test]
    async fn the_broker_url_is_shown_without_its_credentials() {
        let url = "wss://user:s3cret@broker.example/agents?token=t0ken#k3y";
        let report = report(&[("broker_url", url)]).await.join("\n");
        for secret in ["s3cret", "t0ken", "k3y"] {
            assert!(!report.contains(secret), "{secret} in {report}");
        }
        assert!(report.contains("broker.example/agents"), "{report}");
    }
}
