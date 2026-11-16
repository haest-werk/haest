//! The flat frontmatter line reader: `---\n<head>\n---\n<body>` and the
//! `key: value` subset in the head (inline `[a, b]` lists, quoted scalars, and
//! where an indented block starts and ends). Shared by the SKILL.md reader
//! (its fallback when strict YAML refuses) and any format kept as markdown with
//! a head, such as haest-issues' board cards.

/// `---\n<head>\n---\n<body>` -> (head lines, body), or None when there is no block.
/// The head ends at the FIRST `\n---` after the opening fence.
pub fn split_frontmatter(content: &str) -> Option<(Vec<&str>, &str)> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Some((rest[..end].split('\n').collect(), body))
}

/// Nothing at all (a nested block) or a block-scalar indicator (`|`, `>-`, `|+2`).
fn opens_a_block(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['|', '>']) else {
        return value.is_empty();
    };
    let rest = rest.strip_prefix(['-', '+']).unwrap_or(rest);
    rest.bytes().all(|b| b.is_ascii_digit())
}

/// One past the last indented line under `lines[head]` (whose value is
/// `value`), or None when it opens no block. A blank line does not end the
/// block; trailing blanks stay out of it.
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

/// `[a, 'b', ]` -> its items, unquoted, empty ones dropped; None when `value`
/// is not an inline list.
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

/// A matched pair of quotes at both ends is unwrapped; escapes are undone for a
/// DOUBLE-quoted scalar only (a single-quoted one has none). Anything else stays as is.
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
