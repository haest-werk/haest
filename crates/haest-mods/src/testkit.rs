//! This crate's own test kit: the public fakes ([`crate::testing`]) and
//! the mod-file helpers.

use std::{path::Path, time::Duration};

pub(crate) use crate::testing::*;
use crate::{
    env::ModEnv,
    js::{JsMod, ModCaps, ModLoadError},
};

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
