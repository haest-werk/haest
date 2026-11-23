//! How the `[cache]` setting reads and writes itself: as its TOML table, a
//! key left out where it is at its default, so an untouched table writes
//! nothing; `keep_warm_minutes` in minutes; `compact_before_expire` as
//! `"off"` or a table; `min_context` as tokens or a share (`0.6`).

use std::{collections::BTreeMap, fmt, time::Duration};

use haest_core::__private::{BadPattern, DurationIn, TimeUnit, check_pattern, deserialize_in};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeMap,
};

use super::{CacheOverride, CacheSettings, CompactBeforeExpire, MinContext};

/// Every key that differs from the default, in the order the type lists them.
impl Serialize for CacheSettings {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let default = CacheSettings::default();
        let mut map = s.serialize_map(None)?;
        for (key, value, at_default) in [
            ("system", &self.system, default.system),
            ("tools", &self.tools, default.tools),
            ("history", &self.history, default.history),
        ] {
            if *value != at_default {
                map.serialize_entry(key, value)?;
            }
        }
        if self.keep_warm != default.keep_warm {
            map.serialize_entry("keep_warm", &self.keep_warm)?;
        }
        if self.keep_warm_time != default.keep_warm_time {
            let minutes = DurationIn(self.keep_warm_time, TimeUnit::Minutes);
            map.serialize_entry("keep_warm_minutes", &minutes)?;
        }
        if self.compact_before_expire != default.compact_before_expire {
            map.serialize_entry("compact_before_expire", &self.compact_before_expire)?;
        }
        if !self.models.is_empty() {
            map.serialize_entry("models", &self.models)?;
        }
        map.end()
    }
}

/// A duration spelled in minutes (`120`, `1.5`), as every time setting is
/// but a hook's `timeout` (seconds): haest-core's one reader and writer.
pub(super) mod minutes {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        DurationIn(*d, TimeUnit::Minutes).serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        deserialize_in(d, TimeUnit::Minutes)
    }

    /// The same, for a key an override may leave out.
    pub(crate) mod option {
        use super::*;

        pub(crate) fn serialize<S: Serializer>(
            d: &Option<Duration>,
            s: S,
        ) -> Result<S::Ok, S::Error> {
            match d {
                Some(d) => super::serialize(d, s),
                None => s.serialize_none(),
            }
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Option<Duration>, D::Error> {
            super::deserialize(d).map(Some)
        }
    }
}

/// `[models."<pattern>"]`, each key haest's pattern: a model spec, or a
/// prefix ending in `*`. One that is not would match no model, so the
/// override would quietly do nothing.
pub(super) fn models<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, CacheOverride>, D::Error> {
    let models = BTreeMap::<String, CacheOverride>::deserialize(d)?;
    for key in models.keys() {
        match check_pattern(key) {
            Ok(()) => {}
            Err(BadPattern::Empty) => {
                return Err(de::Error::custom(
                    "models.\"\": an empty pattern matches no model",
                ));
            }
            // Not a pattern, and a refusal newer than this match.
            Err(_) => {
                return Err(de::Error::custom(format!(
                    "models.\"{key}\" is not haest's pattern: a model spec (`anthropic:claude-opus-5-5`) or a prefix ending in `*` (`openrouter:google/*`); a regular expression is not read, and `*` only ends a prefix"
                )));
            }
        }
    }
    Ok(models)
}

/// `"off"`, or the table of its conditions.
impl Serialize for CompactBeforeExpire {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CompactBeforeExpire::Off => s.serialize_str("off"),
            CompactBeforeExpire::When {
                min_context,
                instructions,
            } => {
                let mut map = s.serialize_map(None)?;
                map.serialize_entry("min_context", min_context)?;
                if let Some(text) = instructions {
                    map.serialize_entry("instructions", text)?;
                }
                map.end()
            }
        }
    }
}

/// The conditions' table, as a file writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct When {
    min_context: MinContext,
    #[serde(default)]
    instructions: Option<String>,
}

/// `"off"` (or `false`), or `{ min_context = ..., instructions = "..." }`.
impl<'de> Deserialize<'de> for CompactBeforeExpire {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Expected;
        impl<'de> Visitor<'de> for Expected {
            type Value = CompactBeforeExpire;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "\"off\", or a table: { min_context = 120000 or 0.6, instructions = \"...\" }",
                )
            }
            fn visit_str<E: de::Error>(self, word: &str) -> Result<Self::Value, E> {
                match word {
                    "off" => Ok(CompactBeforeExpire::Off),
                    _ => Err(E::invalid_value(de::Unexpected::Str(word), &self)),
                }
            }
            fn visit_bool<E: de::Error>(self, on: bool) -> Result<Self::Value, E> {
                if on {
                    return Err(E::invalid_value(de::Unexpected::Bool(true), &self));
                }
                Ok(CompactBeforeExpire::Off)
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                let when = When::deserialize(MapAccessDeserializer::new(map))?;
                Ok(CompactBeforeExpire::When {
                    min_context: when.min_context,
                    instructions: when.instructions,
                })
            }
        }
        d.deserialize_any(Expected)
    }
}

/// Tokens as a whole number, a share as a fraction (`0.6`).
impl Serialize for MinContext {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MinContext::Tokens(n) => s.serialize_u64(*n),
            MinContext::Share(share) => s.serialize_f64(*share),
        }
    }
}

/// A whole number of tokens, 1 or more, or a share strictly between 0 and 1;
/// anything else (`0`, `1.0`, a percentage as text) is refused, the value
/// saying why.
impl<'de> Deserialize<'de> for MinContext {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Expected;
        impl Visitor<'_> for Expected {
            type Value = MinContext;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a whole number of tokens, 1 or more (120000), or a share of the context window between 0 and 1 (0.6)",
                )
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Self::Value, E> {
                match n {
                    0 => Err(E::invalid_value(de::Unexpected::Unsigned(n), &self)),
                    tokens => Ok(MinContext::Tokens(tokens)),
                }
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Self::Value, E> {
                match u64::try_from(n) {
                    Ok(tokens) => self.visit_u64(tokens),
                    // Below zero: the value itself says what is wrong.
                    Err(_below_zero) => Err(E::invalid_value(de::Unexpected::Signed(n), &self)),
                }
            }
            fn visit_f64<E: de::Error>(self, share: f64) -> Result<Self::Value, E> {
                if share > 0.0 && share < 1.0 {
                    Ok(MinContext::Share(share))
                } else {
                    Err(E::invalid_value(de::Unexpected::Float(share), &self))
                }
            }
        }
        d.deserialize_any(Expected)
    }
}
