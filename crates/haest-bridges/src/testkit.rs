//! This crate's test kit: haest-mods' fakes ([`haest_mods::testing`]) and
//! the mod-file helpers.

use std::{path::Path, time::Duration};

use haest_mods::ModEnv;
pub(crate) use haest_mods::testing::*;

use crate::{JsMod, ModCaps, ModLoadError};

/// Caps short enough that a runaway mod is stopped inside a test.
pub(crate) fn test_caps() -> ModCaps {
    ModCaps {
        slice: Duration::from_millis(300),
        ..ModCaps::default()
    }
}

/// Load `source` as the mod file `file`.
pub(crate) async fn js(file: &str, source: &str, haest: &ModEnv) -> Result<JsMod, ModLoadError> {
    JsMod::load_with(Path::new(file), source.into(), haest, test_caps()).await
}
