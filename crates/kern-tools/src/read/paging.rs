//! A text read, numbered and paged by `offset` / `limit` / `allow_large`, sized
//! to its own budget so the generic spill never touches it.

use kern_core::tool::{JsonMap, ToolContext, ToolError, ToolOutput, opt_bool, opt_num};
use serde_json::json;

const DEFAULT_LINE_LIMIT: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;
/// Claude Code's Read limit, in tokens.
pub const READ_TOKEN_BUDGET: u64 = 25_000;
/// `allow_large` with no known context room (no request cap configured).
const LARGE_FALLBACK_TOKENS: u64 = 100_000;
const CHARS_PER_TOKEN: u64 = 4;

/// How many characters this read may return.
fn budget_chars(allow_large: bool, room: Option<u64>) -> u64 {
    let tokens = if allow_large {
        room.unwrap_or(LARGE_FALLBACK_TOKENS)
    } else {
        READ_TOKEN_BUDGET.min(room.unwrap_or(READ_TOKEN_BUDGET))
    };
    tokens.saturating_mul(CHARS_PER_TOKEN)
}

/// Numbered lines from `start` (1-based), as many as fit `budget` characters (always at least one).
fn numbered_lines(lines: &[&str], start: usize, limit: usize, budget: u64) -> Vec<String> {
    let end = lines.len().min((start - 1).saturating_add(limit));
    let width = end.to_string().len();
    let mut out = Vec::new();
    let mut used: u64 = 0;
    for (i, l) in lines.iter().enumerate().take(end).skip(start - 1) {
        let body = if l.chars().count() > MAX_LINE_CHARS {
            let head: String = l.chars().take(MAX_LINE_CHARS).collect();
            format!("{head}... [line truncated]")
        } else {
            (*l).to_string()
        };
        let line = format!("{:>width$}\t{body}", i + 1);
        let len = line.chars().count() as u64;
        if !out.is_empty() && used + len + 1 > budget {
            break;
        }
        out.push(line);
        used += len + 1;
    }
    out
}

fn continuation(remaining: i128, next: usize, cut_by_budget: bool, allow_large: bool) -> String {
    if remaining <= 0 {
        return String::new();
    }
    let why = if cut_by_budget {
        let limit = if allow_large {
            "context-room"
        } else {
            "25,000-token"
        };
        format!(" (this read hit its {limit} limit)")
    } else {
        String::new()
    };
    let large = if cut_by_budget && !allow_large {
        ", or pass allow_large: true to read them in one call"
    } else {
        ""
    };
    format!("\n\n[{remaining} more lines{why}; continue with offset={next}{large}]")
}

/// A whole-number input of at least 1 (`offset`, `limit`), or `default`.
fn at_least_one(input: &JsonMap, key: &str, default: usize) -> Result<usize, ToolError> {
    Ok(match opt_num(input, key)? {
        // `as` saturates: a huge offset is past the end, a negative one is 1.
        Some(n) => (n.floor() as usize).max(1),
        None => default.max(1),
    })
}

/// A text read, numbered and paged by `offset` / `limit` / `allow_large`. Public
/// for a host's [`super::ReadIntercept`]: a converted document pages like a file.
/// `header` goes in front of the numbered lines; `bytes` is the source's size.
pub fn text_output(
    text: &str,
    bytes: usize,
    input: &JsonMap,
    ctx: &ToolContext,
    header: &str,
) -> Result<ToolOutput, ToolError> {
    let lines: Vec<&str> = text.split('\n').collect();
    let allow_large = opt_bool(input, "allow_large");
    let start = at_least_one(input, "offset", 1)?;
    let default_limit = if allow_large {
        lines.len()
    } else {
        DEFAULT_LINE_LIMIT
    };
    let limit = at_least_one(input, "limit", default_limit)?;
    let budget = budget_chars(allow_large, ctx.context_room_tokens);
    let shown = numbered_lines(&lines, start, limit, budget);
    // i128: an offset past the end (even usize::MAX) stays exact.
    let available = lines.len() as i128 - (start as i128 - 1);
    let cut_by_budget = (shown.len() as i128) < (limit as i128).min(available);
    let next = start.saturating_add(shown.len());
    let remaining = lines.len() as i128 - (next as i128 - 1);
    let tail = continuation(remaining, next, cut_by_budget, allow_large);
    let body = if shown.is_empty() {
        format!("(empty: file has {} lines, offset {start})", lines.len())
    } else {
        shown.join("\n") + &tail
    };
    let content = format!("{header}{body}");
    Ok(ToolOutput {
        result: Some(
            json!({"kind": "file", "mediaType": "text/plain", "text": content, "bytes": bytes}),
        ),
        content,
        sized: true,
        ..Default::default()
    })
}
