//! The Claude Code plugins installed and enabled in Claude Code's dir, as plugin
//! roots for the skill scan. Read from Claude Code's own files, so haest sees the
//! same plugin skills Claude Code would:
//!
//! ```text
//! <claude dir>/plugins/installed_plugins.json  { version: 2, plugins: { "name@marketplace": [{ installPath }] } }
//! <claude dir>/settings.json                   { enabledPlugins: { "name@marketplace": true|false } }
//! ```
//!
//! A plugin absent from enabledPlugins counts as enabled (Claude Code's default).
//! Any unreadable file yields no plugins, never an error.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::scan::PluginRoot;
use crate::fs::Fs;

async fn read_json(fs: &dyn Fs, path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs.read(path).await.ok()?).ok()
}

/// The Claude Code plugins installed under `claude_dir` and not disabled, as
/// roots for the skill scan; empty when there are none or the files cannot be
/// read.
///
/// Reads `plugins/installed_plugins.json` (a plugin's last install wins) and
/// `settings.json`'s `enabledPlugins`: only an explicit `false` turns a
/// plugin off. A plugin's name drops its `@marketplace`.
pub async fn read_installed_plugins(fs: &dyn Fs, claude_dir: &Path) -> Vec<PluginRoot> {
    let installed = read_json(fs, &claude_dir.join("plugins/installed_plugins.json")).await;
    let settings = read_json(fs, &claude_dir.join("settings.json")).await;
    let enabled = settings.as_ref().and_then(|s| s.get("enabledPlugins"));
    let Some(Value::Object(plugins)) = installed.as_ref().and_then(|i| i.get("plugins")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, installs) in plugins {
        if enabled.and_then(|e| e.get(id)) == Some(&Value::Bool(false)) {
            continue;
        }
        let path = installs
            .as_array()
            .and_then(|a| a.last())
            .and_then(|i| i.get("installPath"))
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty());
        if let Some(path) = path {
            let name = id.split('@').next().unwrap_or(id);
            out.push(PluginRoot {
                name: name.into(),
                path: PathBuf::from(path),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::fs::MemFs;

    #[tokio::test]
    async fn lists_installed_plugins_skipping_ones_disabled_in_settings() {
        let installed = json!({
            "version": 2,
            "plugins": {
                "a@mk": [{"installPath": "/p/a/1"}, {"installPath": "/p/a/2"}],
                "b@mk": [{"installPath": "/p/b"}],
                "c@mk": [{}]
            }
        });
        let settings = json!({"enabledPlugins": {"b@mk": false}});
        let fs = MemFs::from_files([
            ("/cfg/plugins/installed_plugins.json", installed.to_string()),
            ("/cfg/settings.json", settings.to_string()),
        ]);
        assert_eq!(
            read_installed_plugins(&fs, Path::new("/cfg")).await,
            [PluginRoot {
                name: "a".into(),
                path: "/p/a/2".into()
            }]
        );
    }

    #[tokio::test]
    async fn a_claude_dir_without_plugins_is_empty_not_an_error() {
        let fs = MemFs::new();
        assert!(
            read_installed_plugins(&fs, Path::new("/missing"))
                .await
                .is_empty()
        );
    }
}
