//! The engine's one built-in host function (`__path`): posix path operations,
//! pure -- Node's `path.posix`, so a script's `path.join` behaves the way its
//! author expects. No file system and no process behind it: `resolve` with no
//! absolute argument resolves from `/`, never from the host's own directory.

use serde_json::{Value, json};

/// Run `op` over `args_json` (a JSON array, every item taken as a string); the result as JSON.
pub(crate) fn path_op(op: &str, args_json: &str) -> Result<String, String> {
    let args: Vec<Value> = serde_json::from_str(args_json).map_err(|e| e.to_string())?;
    let args: Vec<String> = args
        .into_iter()
        .map(|v| match v {
            Value::String(s) => s,
            other => other.to_string(),
        })
        .collect();
    let arg = |i: usize| args.get(i).map_or("", String::as_str);
    let out = match op {
        "join" => json!(join(&args)),
        "resolve" => json!(resolve(&args)),
        "normalize" => json!(normalize(arg(0))),
        "dirname" => json!(dirname(arg(0))),
        "basename" => json!(basename(arg(0), args.get(1).map(String::as_str))),
        "extname" => json!(extname(arg(0))),
        "relative" => json!(relative(arg(0), arg(1))),
        "isAbsolute" => json!(arg(0).starts_with('/')),
        _ => return Err(format!("path.{op} is not available here")),
    };
    Ok(out.to_string())
}

/// Segments with `.` and `..` folded away; a `..` above the start is kept only when allowed.
fn fold(path: &str, allow_above_root: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                } else if allow_above_root {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

pub(crate) fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let folded = fold(path, !absolute);
    match (folded.is_empty(), absolute) {
        (true, true) => "/".into(),
        (true, false) => if trailing { "./" } else { "." }.into(),
        (false, _) => {
            let slash = if trailing { "/" } else { "" };
            let root = if absolute { "/" } else { "" };
            format!("{root}{folded}{slash}")
        }
    }
}

pub(crate) fn join(parts: &[String]) -> String {
    let kept: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    if kept.is_empty() {
        return ".".into();
    }
    normalize(&kept.join("/"))
}

pub(crate) fn resolve(parts: &[String]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for p in parts.iter().rev().chain(std::iter::once(&"/".to_string())) {
        if absolute {
            break;
        }
        if p.is_empty() {
            continue;
        }
        resolved = format!("{p}/{resolved}");
        absolute = p.starts_with('/');
    }
    let folded = fold(&resolved, !absolute);
    if absolute {
        format!("/{folded}")
    } else if folded.is_empty() {
        ".".into()
    } else {
        folded
    }
}

pub(crate) fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let rooted = path.starts_with('/');
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None if rooted => "/".into(),
        None => ".".into(),
        Some(i) => {
            let dir = trimmed[..i].trim_end_matches('/');
            if dir.is_empty() {
                if rooted { "/" } else { "." }.into()
            } else {
                dir.into()
            }
        }
    }
}

fn last_segment(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("")
}

pub(crate) fn basename(path: &str, suffix: Option<&str>) -> String {
    let base = last_segment(path);
    match suffix {
        Some(s) if !s.is_empty() && base != s && base.ends_with(s) => {
            base[..base.len() - s.len()].into()
        }
        _ => base.into(),
    }
}

pub(crate) fn extname(path: &str) -> String {
    let base = last_segment(path);
    match base.rfind('.') {
        Some(i) if i > 0 && base != ".." && !base[..i].chars().all(|c| c == '.') => {
            base[i..].into()
        }
        _ => String::new(),
    }
}

pub(crate) fn relative(from: &str, to: &str) -> String {
    let from = resolve(&[from.to_string()]);
    let to = resolve(&[to.to_string()]);
    if from == to {
        return String::new();
    }
    let segs = |p: &str| -> Vec<String> {
        p.split('/')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    let (f, t) = (segs(&from), segs(&to));
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".into(); f.len() - common];
    out.extend(t[common..].iter().cloned());
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(name: &str, args: Value) -> Value {
        serde_json::from_str(&path_op(name, &args.to_string()).unwrap()).unwrap()
    }

    #[test]
    fn behaves_like_node_path_posix() {
        assert_eq!(op("join", json!(["temp", "out.txt"])), "temp/out.txt");
        assert_eq!(op("join", json!(["/a", "../b", "./c/"])), "/b/c/");
        assert_eq!(op("join", json!([])), ".");
        assert_eq!(op("resolve", json!(["/p", "a", "../b"])), "/p/b");
        assert_eq!(op("resolve", json!(["/p", "/abs/x"])), "/abs/x");
        assert_eq!(op("normalize", json!(["../a/./b/.."])), "../a");
        assert_eq!(op("dirname", json!(["/x/y.js"])), "/x");
        assert_eq!(op("dirname", json!(["y.js"])), ".");
        assert_eq!(op("dirname", json!(["/y.js"])), "/");
        assert_eq!(op("basename", json!(["/x/y.js"])), "y.js");
        assert_eq!(op("basename", json!(["/x/y.js", ".js"])), "y");
        assert_eq!(op("extname", json!(["a/b.test.ts"])), ".ts");
        assert_eq!(op("extname", json!([".bashrc"])), "");
        assert_eq!(op("relative", json!(["/a/b/c", "/a/d"])), "../../d");
        assert_eq!(op("isAbsolute", json!(["/a"])), true);
    }

    #[test]
    fn resolve_never_reaches_the_host_directory() {
        assert_eq!(op("resolve", json!(["a"])), "/a");
    }

    #[test]
    fn an_unknown_op_is_refused() {
        assert!(path_op("readFile", "[]").is_err());
    }
}
