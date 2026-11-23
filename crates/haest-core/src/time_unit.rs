//! A duration a setting spells as a number of a unit (`keep_warm_minutes =
//! 90`, a hook's `timeout = 2.5`): one reader and one writer for every such
//! setting, the unit their parameter.

use std::{fmt, time::Duration};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// What a duration setting's number counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeUnit {
    /// Seconds: a hook's `timeout` only, Claude Code's key copied over as
    /// it is.
    Seconds,
    /// Minutes: every other time setting, the unit in its key
    /// (`max_turn_minutes`, `keep_warm_minutes`).
    Minutes,
}

impl TimeUnit {
    /// How many seconds one is.
    fn seconds(self) -> f64 {
        match self {
            TimeUnit::Seconds => 1.0,
            TimeUnit::Minutes => 60.0,
        }
    }
}

impl fmt::Display for TimeUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TimeUnit::Seconds => "seconds",
            TimeUnit::Minutes => "minutes",
        })
    }
}

/// A duration written as a number of its unit, a whole number when it is
/// one (`90`, `1.5`): what a config file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationIn(pub Duration, pub TimeUnit);

impl Serialize for DurationIn {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let DurationIn(duration, unit) = *self;
        let n = duration.as_secs_f64() / unit.seconds();
        // A whole number from 0 to u64::MAX: the cast keeps it.
        if n.fract() == 0.0 && n <= u64::MAX as f64 {
            s.serialize_u64(n as u64)
        } else {
            s.serialize_f64(n)
        }
    }
}

/// A number of `unit`s (`90`, `1.5`) as a duration. One below 0, not a
/// number, or too long for a [`Duration`] is refused, never read as
/// another.
///
/// # Errors
///
/// `D`'s error, saying the unit: "-5 must be a number of minutes, 0 or more".
pub fn deserialize_in<'de, D: Deserializer<'de>>(
    d: D,
    unit: TimeUnit,
) -> Result<Duration, D::Error> {
    let n = f64::deserialize(d)?;
    (Some(n).filter(|n| n.is_finite() && *n >= 0.0))
        .and_then(|n| Duration::try_from_secs_f64(n * unit.seconds()).ok())
        .ok_or_else(|| de::Error::custom(format!("{n} must be a number of {unit}, 0 or more")))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{DurationIn, TimeUnit, deserialize_in};

    fn read(n: &Value, unit: TimeUnit) -> Result<Duration, String> {
        deserialize_in(n, unit).map_err(|e| e.to_string())
    }

    #[test]
    fn a_number_of_its_unit_reads_and_writes_back_the_same() {
        let minutes = TimeUnit::Minutes;
        assert_eq!(read(&json!(90), minutes), Ok(Duration::from_secs(5_400)));
        assert_eq!(read(&json!(1.5), minutes), Ok(Duration::from_secs(90)));
        assert_eq!(
            read(&json!(2.5), TimeUnit::Seconds),
            Ok(Duration::from_millis(2_500))
        );
        assert_eq!(read(&json!(0), minutes), Ok(Duration::ZERO));
        let written = |d, unit| serde_json::to_value(DurationIn(d, unit)).unwrap();
        assert_eq!(written(Duration::from_secs(5_400), minutes), json!(90));
        assert_eq!(written(Duration::from_secs(90), minutes), json!(1.5));
        assert_eq!(
            written(Duration::from_secs(10), TimeUnit::Seconds),
            json!(10)
        );
    }

    #[test]
    fn below_zero_not_a_number_or_too_long_is_refused_naming_the_unit() {
        for n in [json!(-5), json!(1e300), json!("10")] {
            let why = read(&n, TimeUnit::Minutes).unwrap_err();
            assert!(why.contains("minutes") || why.contains("f64"), "{n}: {why}");
        }
        let why = read(&json!(-1), TimeUnit::Seconds).unwrap_err();
        assert_eq!(why, "-1 must be a number of seconds, 0 or more");
    }
}
