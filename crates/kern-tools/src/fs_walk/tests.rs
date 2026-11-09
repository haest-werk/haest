use std::path::{Path, PathBuf};

use kern_core::fs::MemFs;
use tokio_util::sync::CancellationToken;

use super::{Matcher, WALK_LIMIT, glob_fs, static_prefix};

#[test]
fn the_walk_starts_below_the_segments_with_no_glob_syntax() {
    assert_eq!(
        static_prefix("src/lib/**/*.ts"),
        ("src/lib".into(), "**/*.ts".into())
    );
    assert_eq!(static_prefix("*.ts"), (String::new(), "*.ts".into()));
    assert_eq!(static_prefix("a/b"), ("a".into(), "b".into()));
    assert_eq!(static_prefix("{a,b}/c"), (String::new(), "{a,b}/c".into()));
}

/// The cases the TS kern's matcher (Bun's glob) was probed with.
#[test]
fn matching_follows_the_ts_kern_glob_language() {
    let cases = [
        ("*.ts", ".a.ts", true),
        ("*.ts", "a.ts", true),
        ("*.ts", "d/a.ts", false),
        ("**/*.ts", "a.ts", true),
        ("**/*.ts", "x/y/a.ts", true),
        ("**/*.ts", ".h/a.ts", true),
        ("src/*.ts", "src/a.ts", true),
        ("{a,b}.ts", "b.ts", true),
        ("!*.ts", "a.md", true),
        ("!*.ts", "a.ts", false),
        ("a?.ts", "ab.ts", true),
        ("[ab].ts", "a.ts", true),
        ("**", "x/y", true),
        ("*", "x/y", false),
        ("A.TS", "a.ts", false),
        ("**/b", "a/b", true),
        ("a/**", "a/b/c", true),
        ("a/**/c", "a/c", true),
    ];
    for (pattern, path, want) in cases {
        let m = Matcher::new(pattern).unwrap();
        assert_eq!(m.is_match(path), want, "{pattern} vs {path}");
    }
    assert!(Matcher::new("[abc").is_err());
}

fn sorted(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut v: Vec<_> = paths.into_iter().collect();
    v.sort();
    v
}

#[tokio::test]
async fn the_walk_skips_node_modules_and_git_and_returns_absolute_paths() {
    let fs = MemFs::with_files([
        ("/p/src/a.ts", "x"),
        ("/p/src/deep/b.ts", "x"),
        ("/p/src/c.md", "x"),
        ("/p/node_modules/x/c.ts", "x"),
        ("/p/.git/d.ts", "x"),
    ]);
    let cancel = CancellationToken::new();
    let r = glob_fs(&fs, Path::new("/p"), "**/*.ts", &cancel)
        .await
        .unwrap();
    assert!(!r.capped);
    assert_eq!(
        sorted(r.hits.into_iter().map(|h| h.path)),
        [
            PathBuf::from("/p/src/a.ts"),
            PathBuf::from("/p/src/deep/b.ts")
        ]
    );
    let below = glob_fs(&fs, Path::new("/p"), "src/*.ts", &cancel)
        .await
        .unwrap();
    assert_eq!(
        below.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
        [PathBuf::from("/p/src/a.ts")]
    );
}

#[tokio::test]
async fn an_absolute_pattern_starts_at_its_own_root() {
    let fs = MemFs::with_files([("/p/src/a.ts", "x"), ("/q/b.ts", "x")]);
    let cancel = CancellationToken::new();
    let r = glob_fs(&fs, Path::new("/p"), "/q/*.ts", &cancel)
        .await
        .unwrap();
    assert_eq!(
        r.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
        [PathBuf::from("/q/b.ts")]
    );
}

#[tokio::test]
async fn a_missing_start_directory_finds_nothing_and_does_not_fail() {
    let fs = MemFs::new();
    let r = glob_fs(&fs, Path::new("/nope"), "*.ts", &CancellationToken::new())
        .await
        .unwrap();
    assert!(r.hits.is_empty() && !r.capped);
}

#[tokio::test]
async fn a_walk_past_the_entry_limit_stops_and_says_so() {
    let files = (0..=WALK_LIMIT).map(|i| (format!("/p/f{i}"), ""));
    let fs = MemFs::with_files(files);
    let r = glob_fs(&fs, Path::new("/p"), "nomatch", &CancellationToken::new())
        .await
        .unwrap();
    assert!(r.capped);
}

#[tokio::test]
async fn a_cancelled_walk_stops() {
    let fs = MemFs::with_files([("/p/a.ts", "x")]);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = glob_fs(&fs, Path::new("/p"), "*.ts", &cancel)
        .await
        .unwrap();
    assert!(r.hits.is_empty());
}
