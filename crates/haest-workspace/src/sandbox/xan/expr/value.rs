//! A value, read from a cell and written back to one.

/// A cell, a literal, or what an expression makes of them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Value {
    /// The number this value reads as: a cell is a number when it parses as one.
    pub(crate) fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Str(s) => parse_num(s),
            Value::Null => None,
        }
    }

    pub(crate) fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
        }
    }

    /// Null, or a cell with nothing but spaces.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Str(s) => s.trim().is_empty(),
            _ => false,
        }
    }

    /// As a CSV cell.
    pub(crate) fn text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Num(n) => fmt_num(*n),
            Value::Str(s) => s.clone(),
        }
    }
}

/// A cell as a number. Rust's parser also takes "inf" and "NaN": a cell that
/// says "nan" is text here, so a number needs a digit.
pub(crate) fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if !t.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<f64>().ok()
}

/// A number as a cell: whole numbers without `.0`, and float noise past 15
/// significant digits rounded off (0.1 + 0.2 is 0.3, a sum of money is the sum).
pub(super) fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        return String::new();
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    let rounded: f64 = format!("{n:.14e}").parse().unwrap_or(n);
    format!("{rounded}")
}
