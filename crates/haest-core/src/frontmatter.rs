//! The flat frontmatter line reader: `---\n<head>\n---\n<body>` and the
//! `key: value` subset in the head (inline `[a, b]` lists, quoted scalars, and
//! where an indented block starts and ends). Not a YAML parser: each piece
//! answers one question about one line, and the caller assembles the rest.
//!
//! For a tool that reads a format kept as markdown with a head, such as
//! `haest-issues`' board cards; haest's SKILL.md reader ([`crate::skills`])
//! falls back to it when strict YAML refuses. Start from
//! [`split_frontmatter`].
//!
//! # Examples
//!
//! ```
//! use haest_core::frontmatter::{inline_list, split_frontmatter, unquote};
//!
//! let card = "---\ntitle: \"Fix the \\\"login\\\" bug\"\nlabels: [auth, 'p1']\n---\nSteps to reproduce.\n";
//! let (head, body) = split_frontmatter(card).ok_or("no frontmatter")?;
//! assert_eq!(head, ["title: \"Fix the \\\"login\\\" bug\"", "labels: [auth, 'p1']"]);
//! assert_eq!(body, "Steps to reproduce.\n");
//!
//! let (_, title) = head[0].split_once(": ").ok_or("no title")?;
//! assert_eq!(unquote(title), "Fix the \"login\" bug");
//! let (_, labels) = head[1].split_once(": ").ok_or("no labels")?;
//! assert_eq!(inline_list(labels).unwrap(), ["auth", "p1"]);
//! # Ok::<(), &'static str>(())
//! ```

/// Splits `---\n<head>\n---\n<body>` into the head's lines and the body;
/// None when the first line of `content` is not a fence or the head never ends.
///
/// A fence is a line that is `---` plus trailing blanks (a CRLF line's `\r`
/// is one); `---foo` or `----` is not. The head ends at the FIRST fence after
/// the opening one, and the body starts on the line after it. The head may
/// be empty (`---\n---\n`). Each head line keeps its own bytes: a CRLF file's
/// lines end in `\r`, which trimming a value drops.
pub fn split_frontmatter(content: &str) -> Option<(Vec<&str>, &str)> {
    let mut lines = content.split_inclusive('\n');
    let opener = lines.next().filter(|l| l.ends_with('\n') && is_fence(l))?;
    let mut read = opener.len();
    let mut head = Vec::new();
    for line in lines {
        read += line.len();
        let line = line.strip_suffix('\n').unwrap_or(line);
        if is_fence(line) {
            return Some((head, &content[read..]));
        }
        head.push(line);
    }
    None
}

/// `---`, then nothing but blanks.
fn is_fence(line: &str) -> bool {
    line.trim_end() == "---"
}

/// Nothing at all (a nested block) or a block-scalar indicator (`|`, `>-`, `|+2`).
fn opens_a_block(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['|', '>']) else {
        return value.is_empty();
    };
    let rest = rest.strip_prefix(['-', '+']).unwrap_or(rest);
    rest.bytes().all(|b| b.is_ascii_digit())
}

/// Where the indented block under the key at `lines[head]` ends: one past its
/// last indented line, so `lines[head + 1..end]` is the block. None when the
/// key opens no block.
///
/// `value` is that key's text after the `:`, trimmed. The key opens a block
/// when `value` is empty (a nested list or map follows) or a block-scalar
/// indicator (`|`, `>-`, `|+2`), AND at least one indented line follows. A
/// blank line does not end the block; trailing blanks stay out of it. The
/// next line that is neither blank nor indented ends it.
pub fn block_end(lines: &[&str], head: usize, value: &str) -> Option<usize> {
    if !opens_a_block(value) {
        return None;
    }
    let mut last = head;
    for (i, line) in lines.iter().enumerate().skip(head + 1) {
        let indented = line.starts_with(char::is_whitespace) && !line.trim().is_empty();
        if indented {
            last = i;
        } else if !line.trim().is_empty() {
            break;
        }
    }
    (last != head).then_some(last + 1)
}

/// The items of an inline list, `[a, 'b', ]` -> `["a", "b"]`: each trimmed
/// and [`unquote`]d, empty ones dropped. None when `value` is not wrapped in
/// `[` and `]`.
///
/// Items split on every comma, even one inside quotes.
pub fn inline_list(value: &str) -> Option<Vec<String>> {
    let inner = value.strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(|s| unquote(s.trim()))
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// A scalar without its quotes: a matched pair at both ends is unwrapped.
/// Anything else comes back as is.
///
/// Escapes are undone for a DOUBLE-quoted scalar only: `\n`, `\r`, `\t`,
/// `\xHH`, and a backslash before any other character stands for that
/// character. A single-quoted scalar is taken literally (YAML's `''` is not
/// undone).
pub fn unquote(raw: &str) -> String {
    let mut chars = raw.chars();
    let (Some(q), Some(last)) = (chars.next(), chars.next_back()) else {
        return raw.to_string();
    };
    if (q != '"' && q != '\'') || last != q {
        return raw.to_string();
    }
    let inner = chars.as_str();
    if q == '"' {
        unescape(inner)
    } else {
        inner.to_string()
    }
}

/// The inverse of a double-quoted scalar's escapes, in one pass.
fn unescape(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        let hex = || {
            let mut probe = chars.clone();
            let h: String = probe.by_ref().take(2).collect();
            (h.len() == 2 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| u8::from_str_radix(&h, 16).ok())
                .flatten()
        };
        match e {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'x' => match hex() {
                Some(b) => {
                    out.push(char::from(b));
                    chars.next();
                    chars.next();
                }
                None => out.push('x'),
            },
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_head_ends_at_the_first_fence() {
        let (head, body) = split_frontmatter("---\na: 1\nb: 2\n---\n\nbody\n---\nmore").unwrap();
        assert_eq!(head, ["a: 1", "b: 2"]);
        assert_eq!(body, "\nbody\n---\nmore");
        assert!(split_frontmatter("# no block").is_none());
        assert!(split_frontmatter("---\nunclosed").is_none());
    }

    #[test]
    fn an_empty_head_is_a_head() {
        let (head, body) = split_frontmatter("---\n---\nbody").unwrap();
        assert!(head.is_empty());
        assert_eq!(body, "body");
    }

    #[test]
    fn only_a_line_of_three_dashes_closes_the_head() {
        let (head, body) = split_frontmatter("---\na: 1\n---foo\n---\nbody").unwrap();
        assert_eq!(head, ["a: 1", "---foo"]);
        assert_eq!(body, "body");
        assert!(split_frontmatter("---\na: 1\n----\nbody").is_none());
        let (_, body) = split_frontmatter("---\na: 1\n---").unwrap();
        assert_eq!(body, "");
    }

    #[test]
    fn the_opening_fence_is_a_fence_like_the_closing_one() {
        let (head, body) = split_frontmatter("--- \na: 1\n---\nbody").unwrap();
        assert_eq!(head, ["a: 1"]);
        assert_eq!(body, "body");
        // A CRLF file: each head line keeps its own bytes, `\r` included.
        let (head, body) = split_frontmatter("---\r\na: 1\r\n---\r\nbody\r\n").unwrap();
        assert_eq!(head, ["a: 1\r"]);
        assert_eq!(body, "body\r\n");
        assert!(split_frontmatter("---foo\na: 1\n---\n").is_none());
        assert!(split_frontmatter("----\na: 1\n---\n").is_none());
        assert!(split_frontmatter("---").is_none());
    }

    #[test]
    fn unquote_needs_a_matched_pair() {
        assert_eq!(unquote("'it''"), "it'");
        assert_eq!(unquote("\"unbalanced"), "\"unbalanced");
        assert_eq!(unquote("he said \"hi\""), "he said \"hi\"");
        assert_eq!(unquote("'a\\nb'"), "a\\nb");
    }

    #[test]
    fn a_block_runs_over_indented_and_blank_lines_and_stops_at_the_next_key() {
        let lines = ["refs:", "  - a", "", "  - b", "", "next: 1"];
        assert_eq!(block_end(&lines, 0, ""), Some(4));
        assert_eq!(block_end(&["note: |", "  text"], 0, "|"), Some(2));
        assert_eq!(block_end(&["empty:", "next: 1"], 0, ""), None);
        assert_eq!(block_end(&["a: 1", "  b"], 0, "1"), None);
    }

    #[test]
    fn an_inline_list_is_unquoted_and_drops_empty_items() {
        assert_eq!(
            inline_list("[a, 'b', ]"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(inline_list("[]"), Some(Vec::new()));
        assert_eq!(inline_list("a, b"), None);
    }
}
