//! The unit strings `[timeouts]` and `[limits]` are written in: a duration
//! (`"500ms"`, `"30s"`, `"10m"`, `"1.5h"`) and a size of memory (`"64MiB"`,
//! `"1GiB"`, `"500MB"`). A number alone is refused: `30` could be seconds
//! or minutes, and the setting would quietly be the other.
//!
//! Each reads by decimal arithmetic, never through a float, and writes back
//! in the largest unit that holds it whole (`"90m"` for `"1.5h"`), so what is
//! written reads back the same.

use std::{fmt, time::Duration};

use serde::{
    Deserializer,
    de::{self, Visitor},
};

/// A duration's units, largest first, each in nanoseconds.
const TIME: [(&str, u128); 4] = [
    ("h", 3_600_000_000_000),
    ("m", 60_000_000_000),
    ("s", 1_000_000_000),
    ("ms", 1_000_000),
];

/// The duration units as a refusal lists them.
const TIME_UNITS: &str = "ms, s, m or h";

/// A size's units, largest first, each in bytes. Read in any case
/// (`"64mib"`); written as here.
const SIZE: [(&str, u128); 7] = [
    ("GiB", 1 << 30),
    ("GB", 1_000_000_000),
    ("MiB", 1 << 20),
    ("MB", 1_000_000),
    ("KiB", 1 << 10),
    ("KB", 1_000),
    ("B", 1),
];

/// The size units as a refusal lists them.
const SIZE_UNITS: &str = "B, KB, MB, GB, KiB, MiB or GiB";

/// The least memory a JavaScript engine is given: below it QuickJS cannot
/// start, and every run would fail as out of memory.
pub(crate) const MIN_MEMORY: usize = 1 << 20;

/// `text`'s number and unit: `"1.5h"` is 15 tenths of an hour, read as
/// that many of `scale` (the unit's size, from `units`), truncated to a
/// whole one. None when it is no number followed by one of `units`, or too
/// big for a u128.
fn amount(text: &str, units: &[(&str, u128)], any_case: bool) -> Option<u128> {
    let text = text.trim();
    let end = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = (&text[..end], text[end..].trim_start());
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits_only(fraction) || (number.contains('.') && fraction.is_empty()) {
        return None;
    }
    let (_, scale) = units.iter().find(|(name, _)| {
        if any_case {
            name.eq_ignore_ascii_case(unit)
        } else {
            *name == unit
        }
    })?;
    let places = u32::try_from(fraction.len()).ok()?;
    let digits: u128 = format!("{whole}{fraction}").parse().ok()?;
    digits
        .checked_mul(*scale)?
        .checked_div(10u128.checked_pow(places)?)
}

/// `n` in the largest of `units` that holds it whole, or in the smallest
/// with a fraction when none does.
fn written(n: u128, units: &[(&str, u128)]) -> String {
    if let Some((name, scale)) = units.iter().find(|(_, scale)| n.is_multiple_of(*scale)) {
        return format!("{}{name}", n / scale);
    }
    let (name, scale) = units[units.len() - 1];
    let fraction = format!("{:0width$}", n % scale, width = scale.ilog10() as usize);
    format!("{}.{}{name}", n / scale, fraction.trim_end_matches('0'))
}

/// `text` as a duration above 0: a number and its unit (`"30s"`, `"1.5h"`).
pub(crate) fn duration(text: &str) -> Result<Duration, String> {
    let Some(nanos) = amount(text, &TIME, false) else {
        return Err(format!(
            "{text:?} is no duration: a number and its unit, like \"30s\", \"10m\" or \"1.5h\" ({TIME_UNITS})"
        ));
    };
    if nanos == 0 {
        return Err(format!("{text:?} is no time: a timeout is above 0"));
    }
    let secs = u64::try_from(nanos / 1_000_000_000)
        .map_err(|_too_big| format!("{text:?} is too long for a duration"))?;
    // Below a billion: the cast keeps it.
    Ok(Duration::new(secs, (nanos % 1_000_000_000) as u32))
}

/// `d` as its unit string: `"10m"`, `"30s"`, `"1.5ms"`.
pub(crate) fn duration_text(d: Duration) -> String {
    written(d.as_nanos(), &TIME)
}

/// `text` as memory for a JavaScript engine, [`MIN_MEMORY`] or more: a
/// number and its unit (`"64MiB"`, `"1GiB"`).
pub(crate) fn memory(text: &str) -> Result<usize, String> {
    let Some(bytes) = amount(text, &SIZE, true) else {
        return Err(format!(
            "{text:?} is no size: a number and its unit, like \"64MiB\" or \"1GiB\" ({SIZE_UNITS})"
        ));
    };
    let bytes = usize::try_from(bytes)
        .map_err(|_too_big| format!("{text:?} is more than this machine addresses"))?;
    if bytes < MIN_MEMORY {
        return Err(format!(
            "{text:?} is too little: a JavaScript engine needs 1MiB at least"
        ));
    }
    Ok(bytes)
}

/// `bytes` as its unit string: `"64MiB"`, `"500MB"`.
pub(crate) fn memory_text(bytes: usize) -> String {
    written(bytes as u128, &SIZE)
}

/// What a unit string reads as, for serde: its parser, and what a refusal
/// of a bare number says.
struct UnitString<T> {
    parse: fn(&str) -> Result<T, String>,
    /// "a duration", "a size".
    what: &'static str,
    /// Two examples, `"30s" or "10m"`.
    like: &'static str,
}

impl<T> Visitor<'_> for UnitString<T> {
    type Value = T;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} as text with its unit, like {}", self.what, self.like)
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<T, E> {
        (self.parse)(text).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, n: u64) -> Result<T, E> {
        Err(self.bare(n))
    }

    fn visit_i64<E: de::Error>(self, n: i64) -> Result<T, E> {
        Err(self.bare(n))
    }

    fn visit_f64<E: de::Error>(self, n: f64) -> Result<T, E> {
        Err(self.bare(n))
    }
}

impl<T> UnitString<T> {
    /// A number with no unit is refused, saying how it is written.
    fn bare<E: de::Error>(&self, n: impl fmt::Display) -> E {
        E::custom(format_args!(
            "{n} has no unit: {} is written as text with its unit, like {}",
            self.what, self.like
        ))
    }
}

/// [`duration`] for serde: `"30s"`; a bare number is refused.
pub(crate) fn de_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    d.deserialize_any(UnitString {
        parse: duration,
        what: "a duration",
        like: "\"30s\" or \"10m\" (ms, s, m or h)",
    })
}

/// [`de_duration`] for a key that is off when absent.
pub(crate) fn de_some_duration<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Duration>, D::Error> {
    de_duration(d).map(Some)
}

/// [`memory`] for serde: `"64MiB"`; a bare number is refused.
pub(crate) fn de_memory<'de, D: Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    d.deserialize_any(UnitString {
        parse: memory,
        what: "a size",
        like: "\"64MiB\" or \"1GiB\"",
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_duration_reads_in_each_unit_and_writes_back_in_the_largest_whole_one() {
        let secs = Duration::from_secs;
        for (text, want, back) in [
            ("500ms", Duration::from_millis(500), "500ms"),
            ("30s", secs(30), "30s"),
            ("10m", secs(600), "10m"),
            ("1.5h", secs(5_400), "90m"),
            ("90s", secs(90), "90s"),
            ("120s", secs(120), "2m"),
            ("0.25s", Duration::from_millis(250), "250ms"),
            (" 30 s ", secs(30), "30s"),
            ("1.0005ms", Duration::from_nanos(1_000_500), "1.0005ms"),
        ] {
            let d = duration(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(d, want, "{text}");
            assert_eq!(duration_text(d), back, "{text}");
            assert_eq!(duration(back).unwrap(), d, "{back} reads back");
        }
    }

    #[test]
    fn a_duration_needs_its_unit_and_more_than_nothing() {
        for text in [
            "30", "30sec", "30M", "s", ".5s", "5.s", "1e3s", "-5s", "30 m s", "",
        ] {
            let why = duration(text).unwrap_err();
            assert!(why.contains("is no duration"), "{text}: {why}");
        }
        for text in ["0s", "0.0h", "0.0000001ms"] {
            assert!(duration(text).unwrap_err().contains("above 0"), "{text}");
        }
        let why = duration("99999999999999999999999h").unwrap_err();
        assert!(why.contains("too long"), "{why}");
    }

    #[test]
    fn memory_reads_any_case_and_writes_back_binary_or_decimal_whole() {
        for (text, want, back) in [
            ("64MiB", 64 << 20, "64MiB"),
            ("1GiB", 1 << 30, "1GiB"),
            ("128mib", 128 << 20, "128MiB"),
            ("500MB", 500_000_000, "500MB"),
            ("1.5GiB", 3 << 29, "1536MiB"),
            ("2048KiB", 2 << 20, "2MiB"),
            ("1048577B", (1 << 20) + 1, "1048577B"),
        ] {
            let bytes = memory(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(bytes, want, "{text}");
            assert_eq!(memory_text(bytes), back, "{text}");
            assert_eq!(memory(back).unwrap(), bytes, "{back} reads back");
        }
    }

    #[test]
    fn memory_needs_its_unit_and_a_megabyte() {
        for text in ["64", "64M", "64 megs", "MiB", "-1MiB"] {
            assert!(memory(text).unwrap_err().contains("is no size"), "{text}");
        }
        for text in ["512KiB", "1MB", "0B"] {
            assert!(
                memory(text).unwrap_err().contains("1MiB at least"),
                "{text}"
            );
        }
        assert_eq!(memory("1MiB"), Ok(MIN_MEMORY));
    }

    #[test]
    fn a_bare_number_in_a_file_is_refused_saying_how_to_write_it() {
        #[derive(Debug, serde::Deserialize)]
        struct T {
            #[serde(deserialize_with = "de_duration")]
            t: Duration,
        }
        let read = |text: &str| toml::from_str::<T>(text).map(|t| t.t);
        assert_eq!(read("t = \"30s\"").unwrap(), Duration::from_secs(30));
        let why = read("t = 30").unwrap_err().to_string();
        assert!(
            why.contains("30 has no unit") && why.contains("\"30s\""),
            "{why}"
        );
        let why = read("t = 1.5").unwrap_err().to_string();
        assert!(why.contains("1.5 has no unit"), "{why}");
        let why = read("t = \"30x\"").unwrap_err().to_string();
        assert!(why.contains("\"30x\" is no duration"), "{why}");
    }
}
