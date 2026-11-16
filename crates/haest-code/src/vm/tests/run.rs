//! One run's behaviour (code-vm-run.test.ts `runEngine`): the same code the
//! engine thread runs, driven through [`super::run`] on a private pool.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{caps, host_fn, run, spec};
use crate::vm::{VmCaps, VmSpec};

#[tokio::test]
async fn function_mode_logs_the_returned_value_and_values_set_as_globals() {
    let mut s = VmSpec::default();
    s.values.insert("N".into(), json!(21));
    let code = "console.log('hi', N); console.error('oops'); return { n: N * 2 }";
    let r = run(code, &s, caps(5000), &CancellationToken::new()).await;
    assert_eq!(r.stdout, "hi 21\n");
    assert_eq!(r.stderr, "oops\n");
    let v: Value = serde_json::from_str(r.value.as_deref().unwrap()).unwrap();
    assert_eq!(v, json!({"n": 42}));
}

#[tokio::test]
async fn an_uncaught_error_names_its_line() {
    let r = run(
        "const a = 1\nnull.x()",
        &VmSpec::default(),
        caps(5000),
        &CancellationToken::new(),
    )
    .await;
    let error = r.error.unwrap();
    assert!(error.ends_with("(line 2)"), "{error}");
}

#[tokio::test]
async fn module_mode_imports_what_the_job_provides_and_refuses_the_rest() {
    let s = VmSpec {
        modules: Some(HashMap::from([(
            "m".to_string(),
            "export const v = 7".to_string(),
        )])),
        ..Default::default()
    };
    let never = CancellationToken::new();
    let ok = run(
        "import { v } from 'm'\nconsole.log(v)",
        &s,
        caps(5000),
        &never,
    )
    .await;
    assert_eq!(ok.stdout, "7\n");
    assert_eq!(ok.error, None);
    let missing = run("import x from 'nope'", &s, caps(5000), &never).await;
    assert!(missing.error.is_some());
}

#[tokio::test]
async fn async_calls_resolve_reject_into_the_guest_and_unawaited_ones_finish_first() {
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let late_seen = seen.clone();
    let s = spec([
        (
            "__ok",
            host_fn(|a| async move { Ok(format!("got {}", a[0])) }),
        ),
        ("__bad", host_fn(|_| async { Err("denied".to_string()) })),
        (
            "__late",
            host_fn(move |_| {
                let seen = late_seen.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    seen.lock().unwrap().push("late".into());
                    Ok(String::new())
                }
            }),
        ),
    ]);
    let code = "__late(); let e; try { await __bad() } catch (x) { e = x.message } return [await __ok('a'), e]";
    let r = run(code, &s, caps(5000), &CancellationToken::new()).await;
    let v: Value = serde_json::from_str(r.value.as_deref().unwrap()).unwrap();
    assert_eq!(v, json!(["got a", "denied"]));
    assert_eq!(*seen.lock().unwrap(), vec!["late".to_string()]);
}

#[tokio::test]
async fn a_promise_that_can_never_settle_is_an_error_not_a_hang() {
    let code = "await new Promise(() => {})";
    let r = run(
        code,
        &VmSpec::default(),
        caps(5000),
        &CancellationToken::new(),
    )
    .await;
    assert!(r.error.is_some());
    assert_eq!(r.stopped, None);
}

#[tokio::test]
async fn the_guest_has_no_host_reach_of_its_own() {
    let code = "return [typeof require, typeof process, typeof fetch, typeof setTimeout, typeof std, typeof os]";
    let r = run(
        code,
        &VmSpec::default(),
        caps(5000),
        &CancellationToken::new(),
    )
    .await;
    let v: Value = serde_json::from_str(r.value.as_deref().unwrap()).unwrap();
    assert_eq!(v, json!(vec!["undefined"; 6]));
}

#[tokio::test]
async fn memory_is_capped() {
    let s = VmSpec::default();
    let tight = VmCaps {
        memory_bytes: 8 << 20,
        ..caps(10_000)
    };
    let code = "const a = []; for (;;) a.push('x'.repeat(1024) + a.length)";
    let r = run(code, &s, tight, &CancellationToken::new()).await;
    assert!(r.error.is_some());
    assert_eq!(r.stopped, None);
}
