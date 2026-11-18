//! haest-issues is versioned on its own, so it uses only the public items of
//! the haest crates it depends on (CONTRIBUTING, "One path per item"): a
//! `__private` module is not API, and any patch release of haest-http or
//! haest-core may change it, which would break every build of a released
//! haest-issues at the host's next `cargo update`.

use std::{fs, path::Path};

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_file_reaches_into_a_private_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "examples"] {
        if root.join(dir).is_dir() {
            sources(&root.join(dir), &mut files);
        }
    }
    assert!(
        !files.is_empty(),
        "found no sources under {}",
        root.display()
    );
    let reaching: Vec<String> = files
        .iter()
        .flat_map(|f| {
            let text = fs::read_to_string(f).expect("read a source file");
            let name = f.strip_prefix(root).unwrap_or(f).display().to_string();
            text.lines()
                .enumerate()
                .filter(|(_, l)| l.contains("__private"))
                .map(|(n, l)| format!("{name}:{}: {}", n + 1, l.trim()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        reaching.is_empty(),
        "haest-issues reaches into a private module:\n{}",
        reaching.join("\n")
    );
}
