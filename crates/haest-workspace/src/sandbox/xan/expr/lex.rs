//! An expression's tokens, each with the byte where it starts, and the
//! operator an infix one stands for.

use super::{Bin, Ordering, value::fmt_num};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    End,
}

/// The operators, longest first so `**` is not read as `*` `*`.
const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "=~", "!~", "++", "**", "//", "<", ">", "=", "+", "-", "*",
    "/", "%", "!",
];

/// Each token and the byte where it starts.
pub(super) fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = src[i..].chars().next().expect("in bounds");
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let start = i;
        let tok = if c == '(' {
            i += 1;
            Tok::LParen
        } else if c == ')' {
            i += 1;
            Tok::RParen
        } else if c == ',' {
            i += 1;
            Tok::Comma
        } else if c == '"' || c == '\'' {
            let (s, end) = read_string(src, i)?;
            i = end;
            Tok::Str(s)
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let end = number_end(b, i);
            let n = src[i..end]
                .parse()
                .map_err(|_| format!("not a number: {}", &src[i..end]))?;
            i = end;
            Tok::Num(n)
        } else if c.is_alphabetic() || c == '_' {
            let end = src[i..]
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .map_or(src.len(), |n| i + n);
            let word = &src[i..end];
            i = end;
            Tok::Ident(word.into())
        } else if let Some(op) = OPS.iter().find(|op| src[i..].starts_with(**op)) {
            i += op.len();
            Tok::Op(op)
        } else {
            return Err(format!("unexpected `{c}` at position {i}"));
        };
        out.push((tok, start));
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

fn number_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

fn read_string(src: &str, start: usize) -> Result<(String, usize), String> {
    let quote = src.as_bytes()[start] as char;
    let mut out = String::new();
    let mut chars = src[start + 1..].char_indices();
    while let Some((n, c)) = chars.next() {
        match c {
            _ if c == quote => return Ok((out, start + 1 + n + 1)),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, other)) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Err(format!(
        "a string that starts at position {start} never ends"
    ))
}

/// A token as an error message names it.
pub(super) fn describe(t: &Tok) -> String {
    match t {
        Tok::Num(n) => format!("the number {}", fmt_num(*n)),
        Tok::Str(s) => format!("the string \"{s}\""),
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Op(o) => format!("`{o}`"),
        Tok::LParen => "`(`".into(),
        Tok::RParen => "`)`".into(),
        Tok::Comma => "`,`".into(),
        Tok::End => "the end".into(),
    }
}

/// Binding power of an infix operator, and what it builds.
pub(super) fn infix(t: &Tok) -> Option<(u8, Bin)> {
    use Ordering::*;
    Some(match t {
        Tok::Op("||") => (1, Bin::Or),
        Tok::Ident(w) if w == "or" => (1, Bin::Or),
        Tok::Op("&&") => (2, Bin::And),
        Tok::Ident(w) if w == "and" => (2, Bin::And),
        Tok::Op("==" | "=") => (3, Bin::Cmp(Eq)),
        Tok::Op("!=") => (3, Bin::Cmp(Ne)),
        Tok::Op("<") => (3, Bin::Cmp(Lt)),
        Tok::Op("<=") => (3, Bin::Cmp(Le)),
        Tok::Op(">") => (3, Bin::Cmp(Gt)),
        Tok::Op(">=") => (3, Bin::Cmp(Ge)),
        Tok::Ident(w) => match w.as_str() {
            "eq" => (3, Bin::StrCmp(Eq)),
            "ne" => (3, Bin::StrCmp(Ne)),
            "lt" => (3, Bin::StrCmp(Lt)),
            "le" => (3, Bin::StrCmp(Le)),
            "gt" => (3, Bin::StrCmp(Gt)),
            "ge" => (3, Bin::StrCmp(Ge)),
            _ => return None,
        },
        Tok::Op("+") => (4, Bin::Add),
        Tok::Op("-") => (4, Bin::Sub),
        Tok::Op("++") => (4, Bin::Concat),
        Tok::Op("*") => (5, Bin::Mul),
        Tok::Op("/") => (5, Bin::Div),
        Tok::Op("//") => (5, Bin::IntDiv),
        Tok::Op("%") => (5, Bin::Mod),
        Tok::Op("**") => (7, Bin::Pow),
        _ => return None,
    })
}
