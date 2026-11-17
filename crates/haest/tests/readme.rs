//! The README's quickstart is the crates.io page's and is not compiled; the
//! one in the facade's docs is. They must stay the same code.

use std::{fs, path::Path};

/// The first fenced block after `marker` whose fence line is `fence`, its
/// lines with `prefix` taken off, as a reader sees it: without the lines
/// rustdoc hides (`# `).
fn block(text: &str, marker: &str, fence: &str, prefix: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|l| !l.contains(marker));
    lines.find(|l| l.trim_end() == fence)?;
    let close = format!("{prefix}```");
    // A blank line inside a doc comment is the prefix without its space.
    let code: Vec<&str> = lines
        .take_while(|l| l.trim_end() != close)
        .map(|l| {
            l.strip_prefix(prefix)
                .unwrap_or(l.trim_start_matches(prefix.trim_end()))
        })
        .filter(|l| {
            let l = l.trim_start();
            l != "#" && !l.starts_with("# ")
        })
        .collect();
    Some(code.join("\n"))
}

#[test]
fn the_readme_quickstart_is_the_one_the_docs_compile() -> Result<(), Box<dyn std::error::Error>> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // The workspace's README, or the copy a packaged crate carries.
    let readme = [
        crate_dir.join("../../README.md"),
        crate_dir.join("README.md"),
    ]
    .into_iter()
    .find(|p| p.exists())
    .ok_or("no README.md")?;
    let readme = fs::read_to_string(readme)?;
    let lib = fs::read_to_string(crate_dir.join("src/lib.rs"))?;

    let in_readme =
        block(&readme, "## Embed it", "```rust", "").ok_or("no rust block under Embed it")?;
    let in_docs =
        block(&lib, "//! # Quickstart", "//! ```no_run", "//! ").ok_or("no Quickstart block")?;
    assert_eq!(in_readme, in_docs);
    Ok(())
}
