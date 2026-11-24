//! The app's assets: gpui-kit's component icons, plus the Lucide icons haest-gui
//! uses that are not in that default bundle (an icon missing here renders
//! blank, with no error).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        CircleStop,
        Hand,
        Maximize2,
        OctagonX,
        ShieldAlert,
        Unplug,
        X
    ]
);

/// What the app and the snapshots register.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit_assets::IconName;

    use super::*;

    /// Every `IconName::X` written in src/.
    fn icons_in_source() -> Vec<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (i, _) in text.match_indices("IconName::") {
                        let name: String = text[i + 10..]
                            .chars()
                            .take_while(char::is_ascii_alphanumeric)
                            .collect();
                        if name.starts_with(|c: char| c.is_ascii_uppercase()) && name != "ALL" {
                            out.push(name);
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
            &mut out,
        );
        out.sort();
        out.dedup();
        out
    }

    // Found in the first live screenshots: the Approvals and Interrupt
    // buttons drew no icon, because their icons are not in gpui-kit's
    // default bundle.
    #[test]
    fn every_icon_the_app_names_loads() {
        let names = icons_in_source();
        assert!(names.len() > 10, "found {names:?}");
        let missing: Vec<&String> = names
            .iter()
            .filter(|name| {
                let icon = IconName::ALL
                    .iter()
                    .find(|i| format!("{i:?}") == **name)
                    .unwrap_or_else(|| panic!("no icon named {name}"));
                AppAssets.load(&icon.path()).ok().flatten().is_none()
            })
            .collect();
        assert!(missing.is_empty(), "these icons render blank: {missing:?}");
    }
}
