//! [`Raw`]: a field's value in the checked spelling, and back, for every
//! type a setting's field has.

use std::{path::PathBuf, time::Duration};

use haest_core::{
    Effort, WorkspaceMode, cache::CacheTtl, deferred_tools::DeferMode, provider::WebSearchEngine,
    wind_down::WindDown, words::UnknownWord,
};
use haest_settings::{BuiltinTools, ContextSources, ModReload, SkillSources};

use super::{BrokerAccess, ControlSocket, EnvPassthrough};
use crate::value::{number, parse_bool, split_list};

/// A value in the checked spelling, and back. None = unset.
pub(crate) trait Raw: Sized {
    /// A count: a whole number up to this, never a fraction (`0.5` read as 0
    /// would turn a cap off). None = any number.
    const WHOLE_UP_TO: Option<f64> = None;
    /// The largest number it takes, a fraction or not; None = no bound.
    const UP_TO: Option<f64> = None;
    fn to_raw(&self) -> Option<String>;
    fn from_raw(raw: &str) -> Result<Self, String>;
}

/// A type that already reads and writes itself as one word.
macro_rules! word {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            fn to_raw(&self) -> Option<String> {
                Some(self.as_str().into())
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                raw.parse().map_err(|e: UnknownWord| e.to_string())
            }
        }
    )+};
}
word!(
    Effort,
    CacheTtl,
    WorkspaceMode,
    DeferMode,
    EnvPassthrough,
    ControlSocket,
    BrokerAccess,
    WebSearchEngine,
    ModReload
);

impl Raw for f64 {
    fn to_raw(&self) -> Option<String> {
        Some(self.to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        number(raw, None)
    }
}

macro_rules! count {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            const WHOLE_UP_TO: Option<f64> = Some(<$t>::MAX as f64);
            fn to_raw(&self) -> Option<String> {
                Some(self.to_string())
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                number(raw, Self::WHOLE_UP_TO).map(|n| n as $t)
            }
        }
    )+};
}
count!(u32, u64, usize);

macro_rules! words {
    ($($t:ty),+) => {$(
        impl Raw for $t {
            fn to_raw(&self) -> Option<String> {
                Some(self.words().join(","))
            }
            fn from_raw(raw: &str) -> Result<Self, String> {
                let words = split_list(raw);
                <$t>::from_words(words.iter().map(String::as_str)).map_err(|e| e.to_string())
            }
        }
    )+};
}
words!(BuiltinTools, ContextSources, SkillSources);

impl Raw for bool {
    fn to_raw(&self) -> Option<String> {
        Some(self.to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        parse_bool(raw).ok_or_else(|| format!("\"{raw}\" must be true or false"))
    }
}

impl Raw for String {
    fn to_raw(&self) -> Option<String> {
        Some(self.clone())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(raw.into())
    }
}

impl Raw for PathBuf {
    fn to_raw(&self) -> Option<String> {
        Some(self.display().to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(raw.into())
    }
}

impl Raw for Vec<String> {
    fn to_raw(&self) -> Option<String> {
        Some(self.join(","))
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        Ok(split_list(raw))
    }
}

/// A wall-clock cap, in minutes as every time setting spells it (a fraction
/// too; 0 = no cap). One too long for a `Duration` is the longest there is.
impl Raw for Duration {
    fn to_raw(&self) -> Option<String> {
        Some((self.as_secs_f64() / 60.0).to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        let minutes = number(raw, None)?;
        Ok(Duration::try_from_secs_f64(minutes * 60.0).unwrap_or(Duration::MAX))
    }
}

/// A share from 0 to 1, 0 = off.
impl Raw for WindDown {
    const UP_TO: Option<f64> = Some(1.0);
    fn to_raw(&self) -> Option<String> {
        Some(self.share().to_string())
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        WindDown::at(number(raw, None)?).map_err(|e| e.to_string())
    }
}

impl<T: Raw> Raw for Option<T> {
    const WHOLE_UP_TO: Option<f64> = T::WHOLE_UP_TO;
    const UP_TO: Option<f64> = T::UP_TO;
    fn to_raw(&self) -> Option<String> {
        self.as_ref().and_then(T::to_raw)
    }
    fn from_raw(raw: &str) -> Result<Self, String> {
        T::from_raw(raw).map(Some)
    }
}
