//! Override KEYS for OpenRouter provider routing: an exact model id
//! (`z-ai/glm-4.6`), a glob family (`z-ai/glm-*`), or an author prefix (`z-ai/`).
//! How a key matches a kern model, how specific it is, and why a key is refused.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Model,
    Family,
}

/// A pattern is a FAMILY when it can match more than one model.
pub fn match_kind(pattern: &str) -> MatchKind {
    if pattern.contains('*') || pattern.ends_with('/') {
        MatchKind::Family
    } else {
        MatchKind::Model
    }
}

/// Why a pattern cannot be an override key, or None when it can.
pub fn pattern_refusal(pattern: &str) -> Option<&'static str> {
    let p = pattern.trim();
    if p.is_empty() {
        return Some("Enter a model id or a family pattern");
    }
    if p.chars().any(char::is_whitespace) {
        return Some("No spaces: a model id looks like z-ai/glm-4.6");
    }
    if p.contains(':') {
        return Some("Use the OpenRouter model id without the openrouter: prefix or a :variant");
    }
    if !p.contains('/') {
        return Some("Include the author: z-ai/ or z-ai/glm-*");
    }
    if p.chars().all(|c| c == '*' || c == '/') {
        return Some("Too broad: that is the default rule");
    }
    None
}

/// The kern model id stripped to what an override matches: no `openrouter:` spec prefix, no `:variant`.
fn routing_model_id(model: &str) -> &str {
    let bare = model.strip_prefix("openrouter:").unwrap_or(model);
    bare.split_once(':').map_or(bare, |(id, _)| id)
}

/// `*` matches any run of characters; everything else is literal; anchored at both ends.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, rest) = parts.split_first().expect("split yields at least one part");
    let Some(mut tail) = text.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return tail.is_empty();
    };
    for part in middle {
        match tail.find(part) {
            Some(i) => tail = &tail[i + part.len()..],
            None => return false,
        }
    }
    tail.len() >= last.len() && tail.ends_with(last)
}

pub fn pattern_matches(pattern: &str, model: &str) -> bool {
    let id = routing_model_id(model);
    if pattern.contains('*') {
        return glob_matches(pattern, id);
    }
    if pattern.ends_with('/') {
        return id.starts_with(pattern);
    }
    id == pattern
}

/// Specificity rank: exact models first, then families by literal length (longer = narrower).
pub fn specificity(pattern: &str) -> usize {
    let literal = pattern.chars().filter(|c| *c != '*').count();
    match match_kind(pattern) {
        MatchKind::Model => 1_000_000 + literal,
        MatchKind::Family => literal,
    }
}
