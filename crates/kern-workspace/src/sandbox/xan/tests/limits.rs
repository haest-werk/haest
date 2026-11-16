//! What a hostile or runaway command line cannot do to the process xan runs
//! in: overflow its stack.

use kern_core::{shell_command::ShellCommand, workspace::ExecResult};

use super::{Xan, call, ok};
use crate::sandbox::xan::expr::{MAX_DEPTH, MAX_NESTING};

/// `xan args` over sales.csv on a fresh thread with a tokio worker's stack
/// (2 MiB), where a host runs it. An overflow aborts the whole test binary.
fn on_a_worker_stack(args: &[&str]) -> ExecResult {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(Xan.run(call(&args, None)))
        })
        .unwrap()
        .join()
        .unwrap()
}

fn refused(args: &[&str]) {
    let r = on_a_worker_stack(args);
    assert_eq!(r.exit_code, 1, "{}", r.stderr);
    assert!(r.stdout.is_empty());
}

#[test]
fn an_expression_nested_without_end_is_refused_not_a_crash() {
    const N: usize = 100_000;
    let filter = |e: String| refused(&["filter", &e, "sales.csv"]);
    filter("!".repeat(N) + "amount");
    filter("0 + ".to_string() + &"-".repeat(N) + "amount");
    filter("not ".repeat(N) + "amount");
    filter("(".repeat(N) + "amount" + &")".repeat(N));
    filter("abs(".repeat(N) + "amount" + &")".repeat(N));
    filter("2 ** ".repeat(N) + "amount");
    // No recursion in the parser, but the tree it builds is as deep.
    filter("amount".to_string() + &" + 1".repeat(N));
    filter("amount == 1 or ".repeat(N) + "false");
    filter("amount".to_string() + &" =~ \"1\"".repeat(N));
    let agg = |e: String| refused(&["agg", &e, "sales.csv"]);
    agg(format!("sum({}amount)", "-".repeat(N)));
    agg("count()".to_string() + &" + count()".repeat(N));
}

/// Each way the parser recurses, `k` deep.
fn nested(k: usize) -> Vec<String> {
    vec![
        "!".repeat(k) + "amount",
        "(".repeat(k) + "amount" + &")".repeat(k),
        "abs(".repeat(k) + "amount" + &")".repeat(k),
        "2 ** ".repeat(k) + "amount",
    ]
}

/// A chain `k` operators long.
fn chain(k: usize) -> String {
    "amount".to_string() + &" + 1".repeat(k)
}

#[test]
fn the_deepest_expression_allowed_runs_on_a_worker_stack() {
    let runs = |e: String| {
        let r = on_a_worker_stack(&["filter", &e, "sales.csv"]);
        assert_eq!(r.exit_code, 0, "{}...: {}", &e[..20], r.stderr);
    };
    nested(MAX_NESTING - 1).into_iter().for_each(runs);
    runs(chain(MAX_DEPTH - 1));
    for e in nested(MAX_NESTING) {
        refused(&["filter", &e, "sales.csv"]);
    }
    refused(&["filter", &chain(MAX_DEPTH), "sales.csv"]);
}

#[tokio::test]
async fn real_expressions_nest_as_deep_as_they_need() {
    let paid = ok(&[
        "filter",
        &("(".repeat(50) + "status eq \"paid\"" + &")".repeat(50)),
        "sales.csv",
    ])
    .await;
    assert_eq!(paid.lines().count(), 5, "{paid}");
    // A long list of alternatives: one `or` deeper each.
    let any: Vec<String> = (0..300).map(|i| format!("amount == {i}")).collect();
    let r = ok(&["filter", &any.join(" or "), "sales.csv"]).await;
    assert_eq!(r.lines().count(), 4, "{r}");
}
