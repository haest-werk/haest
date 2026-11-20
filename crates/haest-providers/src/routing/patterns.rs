//! Override KEYS for OpenRouter provider routing: an exact model id
//! (`z-ai/glm-4.6`), a glob family (`z-ai/glm-*`), or an author prefix (`z-ai/`).
//! How a key matches a haest model, how specific it is, and which keys are refused.

/// Whether `pattern` can be an override key: an OpenRouter id with its
/// author (no spaces, no `openrouter:` prefix or `:variant`), and never one
/// that matches every model (that is the default rule).
pub(crate) fn is_override_key(pattern: &str) -> bool {
    let p = pattern.trim();
    !p.is_empty()
        && !p.chars().any(char::is_whitespace)
        && !p.contains(':')
        && p.contains('/')
        && !p.chars().all(|c| c == '*' || c == '/')
}

/// The haest model id stripped to what an override matches: no `openrouter:` spec prefix, no `:variant`.
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

/// Whether override `pattern` covers `model`.
///
/// `model` may be an OpenRouter id or a haest spec: an `openrouter:` prefix and
/// a `:variant` suffix are ignored. A glob's `*` matches any run of
/// characters, anchored at both ends; a pattern ending in `/` matches by
/// prefix; any other must equal the id.
pub(crate) fn pattern_matches(pattern: &str, model: &str) -> bool {
    let id = routing_model_id(model);
    if pattern.contains('*') {
        return glob_matches(pattern, id);
    }
    if pattern.ends_with('/') {
        return id.starts_with(pattern);
    }
    id == pattern
}

/// Specificity rank: exact models first, then families (a glob or an author
/// prefix, matching more than one model) by literal length (longer = narrower).
pub(crate) fn specificity(pattern: &str) -> usize {
    let literal = pattern.chars().filter(|c| *c != '*').count();
    let family = pattern.contains('*') || pattern.ends_with('/');
    if family { literal } else { 1_000_000 + literal }
}
