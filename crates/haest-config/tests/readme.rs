//! The README's Rust blocks are not compiled where they stand. Its
//! quickstart is the facade's docs' (crates/haest/tests/readme.rs holds the
//! two together); every other block is a doctest in src/readme.rs, which
//! the gates compile. They must stay the same code.

use std::{fs, path::Path};

/// The fenced blocks of `text` whose fence line is `fence`, each line with
/// `prefix` taken off, and without the lines a doctest hides (`# ...`).
fn blocks(text: &str, fence: &str, prefix: &str) -> Vec<String> {
    let close = format!("{prefix}```");
    let mut out = Vec::new();
    let mut lines = text.lines();
    while lines.any(|l| l.trim_end() == fence) {
        let code: Vec<&str> = lines
            .by_ref()
            .take_while(|l| l.trim_end() != close)
            // A blank line inside a doc comment is the prefix without its space.
            .map(|l| {
                l.strip_prefix(prefix)
                    .unwrap_or(l.trim_start_matches(prefix.trim_end()))
            })
            .filter(|l| *l != "#" && !l.starts_with("# "))
            .collect();
        out.push(code.join("\n"));
    }
    out
}

#[test]
fn every_readme_block_but_the_quickstart_is_a_doctest() -> Result<(), Box<dyn std::error::Error>> {
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
    let compiled = fs::read_to_string(crate_dir.join("src/readme.rs"))?;

    let in_readme = blocks(&readme, "```rust", "");
    let (quickstart, rest) = in_readme
        .split_first()
        .ok_or("no rust block in the README")?;
    assert!(quickstart.contains("async fn main"), "{quickstart}");
    assert_eq!(rest, blocks(&compiled, "/// ```", "/// "));
    Ok(())
}
