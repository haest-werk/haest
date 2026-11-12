//! Columns by name: xan's selection (`name,age`, `a:c`, `0,2`, `!notes`, `*`,
//! `"a, b"` for a name with a comma).

/// The column called `name`: exactly, else the one that matches ignoring case
/// and surrounding spaces (a header "Amount " is still `amount`).
pub fn resolve_name(headers: &[String], name: &str) -> Option<usize> {
    if let Some(i) = headers.iter().position(|h| h == name) {
        return Some(i);
    }
    let want = name.trim().to_lowercase();
    let mut loose = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().to_lowercase() == want);
    match (loose.next(), loose.next()) {
        (Some((i, _)), None) => Some(i),
        _ => None,
    }
}

/// The headers for an error message, at most 30.
pub fn column_list(headers: &[String]) -> String {
    let mut shown: Vec<String> = headers.iter().take(30).map(|h| format!("`{h}`")).collect();
    if headers.len() > 30 {
        shown.push(format!("and {} more (xan headers)", headers.len() - 30));
    }
    if shown.is_empty() {
        return "none (the input is empty)".into();
    }
    shown.join(", ")
}

/// A name, else a 0-based index.
fn one(headers: &[String], item: &str, quoted: bool) -> Result<usize, String> {
    if let Some(i) = resolve_name(headers, item) {
        return Ok(i);
    }
    if !quoted && let Ok(i) = item.trim().parse::<usize>() {
        if i < headers.len() {
            return Ok(i);
        }
        return Err(format!(
            "column index {i} is past the last column ({})",
            headers.len().saturating_sub(1)
        ));
    }
    Err(format!(
        "no column `{item}`; the columns are {}",
        column_list(headers)
    ))
}

/// One item of a selection.
#[derive(Default)]
struct Item {
    text: String,
    /// Written in double quotes: a name, never an index or a range.
    quoted: bool,
    /// Led by `!`: leave these out.
    negate: bool,
}

/// The items of a selection, split on commas outside double quotes.
fn items(spec: &str) -> Vec<Item> {
    let mut out = Vec::new();
    let mut cur = Item::default();
    let mut in_quotes = false;
    for c in spec.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.quoted = true;
            }
            ',' if !in_quotes => out.push(std::mem::take(&mut cur)),
            '!' if !in_quotes && !cur.quoted && cur.text.trim().is_empty() => cur.negate = true,
            _ => cur.text.push(c),
        }
    }
    out.push(cur);
    out
}

/// The columns `spec` picks, in its order (a column named twice comes twice).
/// Items starting with `!` leave columns out: alone they mean "all but these".
pub fn select(spec: &str, headers: &[String]) -> Result<Vec<usize>, String> {
    let mut picked = Vec::new();
    let mut dropped = Vec::new();
    let mut any_positive = false;
    for Item {
        text,
        quoted,
        negate,
    } in items(spec)
    {
        let item = if quoted {
            text
        } else {
            text.trim().to_string()
        };
        if item.is_empty() && !quoted {
            continue;
        }
        let cols = if quoted {
            vec![one(headers, &item, true)?]
        } else if item == "*" {
            (0..headers.len()).collect()
        } else if resolve_name(headers, &item).is_none()
            && let Some((a, b)) = item.split_once(':')
        {
            let from = if a.trim().is_empty() {
                0
            } else {
                one(headers, a, false)?
            };
            let to = if b.trim().is_empty() {
                headers.len().saturating_sub(1)
            } else {
                one(headers, b, false)?
            };
            if from <= to {
                (from..=to).collect()
            } else {
                (to..=from).rev().collect()
            }
        } else {
            vec![one(headers, &item, false)?]
        };
        if negate {
            dropped.extend(cols);
        } else {
            any_positive = true;
            picked.extend(cols);
        }
    }
    if !any_positive {
        picked = (0..headers.len()).collect();
    }
    picked.retain(|i| !dropped.contains(i));
    if picked.is_empty() {
        return Err(format!("`{spec}` selects no column"));
    }
    Ok(picked)
}
