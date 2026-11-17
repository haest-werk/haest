use std::sync::Mutex;

use super::*;

#[tokio::test]
async fn a_workspace_that_cannot_stream_hands_the_output_over_when_it_ends() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let into = seen.clone();
    let opts = ExecOptions::new("/", Duration::from_secs(1));
    let r = ShutWorkspace
        .shell_streaming(
            "true",
            opts,
            OutputChunks::from(move |s: &str| into.lock().unwrap().push(s.into())),
        )
        .await;
    assert_eq!(*seen.lock().unwrap(), [NO_SHELL]);
    assert_eq!(r.exit_code, EXIT_NOT_FOUND);
    assert!(r.stdout.is_empty() && r.stderr.is_empty());
}

/// The TS haest's `ApplyResult`: a failed change is the change's own keys
/// plus `code` and `reason`.
#[test]
fn an_apply_report_has_the_ts_haests_wire_shape() {
    let change = Change::new("/l", ChangeKind::Added);
    let report = ApplyReport {
        applied: vec![],
        skipped: vec![],
        failed: vec![FailedChange::new(change, ApplyFailureCode::Symlink, "why")],
    };
    let wire = serde_json::json!({
        "applied": [],
        "skipped": [],
        "failed": [{"path": "/l", "kind": "added", "conflict": false, "code": "symlink", "reason": "why"}],
    });
    assert_eq!(serde_json::to_value(&report).unwrap(), wire);
    assert_eq!(serde_json::from_value::<ApplyReport>(wire).unwrap(), report);
    let words = [
        (ApplyFailureCode::OutsideProject, "outside-project"),
        (ApplyFailureCode::Symlink, "symlink"),
        (ApplyFailureCode::WriteFailed, "write-failed"),
    ];
    for (code, word) in words {
        assert_eq!(serde_json::to_value(code).unwrap(), word);
    }
}
