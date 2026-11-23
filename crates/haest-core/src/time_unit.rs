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
    /// Minutes: every other time setting that takes a number, the unit in
    /// its key (`max_turn_minutes`, `keep_warm_minutes`). The cache's TTLs
    /// are words (`5m`, `1h`), not numbers.
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

/// Why a number is no duration ([`duration_in`]). The number is shown as
/// written when it is short, and as `1e300` when it is huge.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum BadDuration {
    /// Below 0, or not a number at all (NaN).
    #[error("{} must be a number of {unit}, 0 or more", Shown(*n))]
    BelowZero {
        /// The number, as read.
        n: f64,
        /// What it counts.
        unit: TimeUnit,
    },
    /// Longer than a [`Duration`] holds (infinity too): read as the
    /// longest there is, it would be a number the human never wrote.
    #[error(
        "{} {unit} is too long: a duration holds at most about 584 billion years",
        Shown(*n)
    )]
    TooLong {
        /// The number, as read.
        n: f64,
        /// What it counts.
        unit: TimeUnit,
    },
}

/// A number in an error: as Rust writes it, or in exponent form (`1e300`)
/// from a quadrillion up, where Rust would write every digit.
struct Shown(f64);

impl fmt::Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Shown(n) = *self;
        if n.is_finite() && n.abs() >= 1e15 {
            write!(f, "{n:e}")
        } else {
            write!(f, "{n}")
        }
    }
}

/// `n` `unit`s (`90`, `1.5`) as a duration: the one rule every duration
/// setting reads its number by, from a file, a variable or a flag. One
/// below 0, NaN, or too long for a [`Duration`] is refused, never read as
/// another.
///
/// # Errors
///
/// [`BadDuration`]: below 0 or NaN, or too long.
pub fn duration_in(n: f64, unit: TimeUnit) -> Result<Duration, BadDuration> {
    if n.is_nan() || n < 0.0 {
        return Err(BadDuration::BelowZero { n, unit });
    }
    Duration::try_from_secs_f64(n * unit.seconds())
        .map_err(|_too_long| BadDuration::TooLong { n, unit })
}

/// [`duration_in`] for serde: a number of `unit`s (`90`, `1.5`) as a
/// duration.
///
/// # Errors
///
/// `D`'s error: not a number, or [`BadDuration`]'s text ("-5 must be a
/// number of minutes, 0 or more").
pub fn deserialize_in<'de, D: Deserializer<'de>>(
    d: D,
    unit: TimeUnit,
) -> Result<Duration, D::Error> {
    duration_in(f64::deserialize(d)?, unit).map_err(de::Error::custom)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{BadDuration, DurationIn, TimeUnit, deserialize_in, duration_in};

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
    fn below_zero_or_not_a_number_is_refused_naming_the_unit() {
        let minutes = TimeUnit::Minutes;
        for n in [-5.0, -1e300, f64::NAN, f64::NEG_INFINITY] {
            let refused = duration_in(n, minutes);
            assert!(
                matches!(refused, Err(BadDuration::BelowZero { .. })),
                "{n}: {refused:?}"
            );
        }
        assert!(read(&json!("10"), minutes).is_err(), "text is no number");
        let why = read(&json!(-1), TimeUnit::Seconds).unwrap_err();
        assert_eq!(why, "-1 must be a number of seconds, 0 or more");
        let why = duration_in(-1e300, minutes).unwrap_err().to_string();
        assert!(why.starts_with("-1e300 "), "{why}");
    }

    /// A number too long for a `Duration` is refused as too long, never read
    /// as the longest there is; up to that, it reads.
    #[test]
    fn too_long_for_a_duration_is_refused_as_too_long() {
        let minutes = TimeUnit::Minutes;
        // Duration::MAX is 3.07e17 minutes.
        assert!(duration_in(3.0e17, minutes).is_ok());
        for n in [3.1e17, 1e300, f64::INFINITY] {
            let refused = duration_in(n, minutes);
            assert!(
                matches!(refused, Err(BadDuration::TooLong { .. })),
                "{n}: {refused:?}"
            );
        }
        let in_seconds = duration_in(1.9e19, TimeUnit::Seconds);
        assert!(matches!(in_seconds, Err(BadDuration::TooLong { .. })));
        let why = read(&json!(1e300), minutes).unwrap_err();
        assert!(
            why.starts_with("1e300 minutes is too long") && why.len() < 100,
            "{why}"
        );
    }
}
