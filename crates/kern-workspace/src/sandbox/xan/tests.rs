use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use kern_core::{
    fs::MemFs,
    shell_command::{ShellCall, ShellCommand},
    workspace::ExecResult,
};
use tokio_util::sync::CancellationToken;

use super::Xan;

const SALES: &str = "\
region,product,amount,status,note
north,apple,120,paid,
south,pear,80,open,\"late, again\"
north,pear,45.5,paid,
east,apple,n/a,paid,missing
south,apple,200,paid,
";

/// `xan args` in `/data` (sales.csv, t.tsv), 10s from its deadline.
fn call(args: &[&str], stdin: Option<&str>) -> ShellCall {
    let fs = MemFs::with_files([("/data/sales.csv", SALES), ("/data/t.tsv", "a\tb\n1\t2\n")]);
    ShellCall {
        args: args.iter().map(|a| a.to_string()).collect(),
        cwd: PathBuf::from("/data"),
        env: Default::default(),
        stdin: stdin.map(|s| s.as_bytes().to_vec()),
        fs: Arc::new(fs),
        cancel: CancellationToken::new(),
        ends_at: Instant::now() + Duration::from_secs(10),
    }
}

async fn xan(args: &[&str], stdin: Option<&str>) -> ExecResult {
    Xan.run(call(args, stdin)).await
}

async fn ok_stdin(args: &[&str], stdin: &str) -> String {
    ok_in(args, Some(stdin)).await
}

async fn ok(args: &[&str]) -> String {
    ok_in(args, None).await
}

async fn ok_in(args: &[&str], stdin: Option<&str>) -> String {
    let r = xan(args, stdin).await;
    assert_eq!(r.exit_code, 0, "xan {args:?} failed: {}", r.stderr);
    r.stdout
}

#[tokio::test]
async fn headers_are_numbered_from_zero_and_count_leaves_the_header_out() {
    assert_eq!(
        ok(&["headers", "sales.csv"]).await,
        "0   region\n1   product\n2   amount\n3   status\n4   note\n"
    );
    assert_eq!(
        ok(&["headers", "-j", "sales.csv"]).await,
        "region\nproduct\namount\nstatus\nnote\n"
    );
    assert_eq!(ok(&["count", "sales.csv"]).await, "5\n");
    assert_eq!(
        ok(&["head", "-l", "1", "sales.csv"]).await,
        "region,product,amount,status,note\nnorth,apple,120,paid,\n"
    );
}

#[tokio::test]
async fn select_takes_names_ranges_indices_and_leaves_columns_out() {
    let first = |s: String| s.lines().take(2).collect::<Vec<_>>().join("\n");
    assert_eq!(
        first(ok(&["select", "amount,region", "sales.csv"]).await),
        "amount,region\n120,north"
    );
    assert_eq!(
        first(ok(&["select", "product:status", "sales.csv"]).await),
        "product,amount,status\napple,120,paid"
    );
    assert_eq!(
        first(ok(&["select", "0,2", "sales.csv"]).await),
        "region,amount\nnorth,120"
    );
    assert_eq!(
        first(ok(&["select", "!note,!status", "sales.csv"]).await),
        "region,product,amount\nnorth,apple,120"
    );
    // A quoted field with a comma stays one field.
    assert!(
        ok(&["select", "note", "sales.csv"])
            .await
            .contains("\"late, again\"")
    );
    let r = xan(&["select", "price", "sales.csv"], None).await;
    assert_eq!(r.exit_code, 1);
    assert!(
        r.stderr.contains("no column `price`") && r.stderr.contains("`amount`"),
        "{}",
        r.stderr
    );
}

#[tokio::test]
async fn filter_compares_numbers_as_numbers_and_text_as_text() {
    // 45.5 < 100 < 120: as text "45.5" > "120" would be true.
    let big = ok(&["filter", "amount > 100", "sales.csv"]).await;
    assert_eq!(
        big,
        "region,product,amount,status,note\nnorth,apple,120,paid,\nsouth,apple,200,paid,\n"
    );
    let r = ok(&[
        "filter",
        "status eq \"paid\" and product == 'pear'",
        "sales.csv",
    ])
    .await;
    assert_eq!(r.lines().count(), 2, "{r}");
    assert!(r.contains("45.5"));
    // n/a is not a number: the arithmetic is empty, the row drops out.
    assert_eq!(
        ok(&["filter", "amount * 2 > 0", "sales.csv"])
            .await
            .lines()
            .count(),
        5
    );
    assert_eq!(
        ok(&["filter", "-v", "is_numeric(amount)", "sales.csv"])
            .await
            .lines()
            .nth(1),
        Some("east,apple,n/a,paid,missing")
    );
    assert_eq!(
        ok(&["filter", "region =~ \"^n\"", "-l", "1", "sales.csv"])
            .await
            .lines()
            .count(),
        2
    );
    assert_eq!(
        ok(&["filter", "contains(lower(note), 'late')", "sales.csv"])
            .await
            .lines()
            .count(),
        2
    );
    assert_eq!(
        ok(&[
            "filter",
            "col(\"amount\") >= 80 && !is_empty(note)",
            "sales.csv"
        ])
        .await
        .lines()
        .count(),
        2
    );
}

#[tokio::test]
async fn a_bad_expression_says_what_is_wrong_before_any_row() {
    for (expr, says) in [
        ("amout > 1", "no column `amout`"),
        ("amount >", "expected a value"),
        ("sum(amount) > 1", "an aggregation"),
        ("amount > 1 status", "unexpected"),
        ("'open", "never ends"),
    ] {
        let r = xan(&["filter", expr, "sales.csv"], None).await;
        assert_eq!(r.exit_code, 1, "{expr}");
        assert!(r.stderr.contains(says), "{expr}: {}", r.stderr);
    }
}

#[tokio::test]
async fn sort_numbers_as_numbers_reversed_and_stable() {
    let amounts = |s: String| {
        s.lines()
            .skip(1)
            .map(|l| l.split(',').nth(2).unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        amounts(ok(&["sort", "-s", "amount", "-N", "sales.csv"]).await),
        ["45.5", "80", "120", "200", "n/a"]
    );
    assert_eq!(
        amounts(ok(&["sort", "-s", "amount", "-NR", "sales.csv"]).await),
        ["n/a", "200", "120", "80", "45.5"]
    );
    // Without -N, as text.
    assert_eq!(
        amounts(ok(&["sort", "-s", "amount", "sales.csv"]).await),
        ["120", "200", "45.5", "80", "n/a"]
    );
    // Ties keep their order: north's apple before north's pear.
    let regions = ok(&["sort", "-s", "region", "sales.csv"]).await;
    let north: Vec<&str> = regions.lines().filter(|l| l.starts_with("north")).collect();
    assert_eq!(north, ["north,apple,120,paid,", "north,pear,45.5,paid,"]);
}

#[tokio::test]
async fn groupby_aggregates_each_group_in_order_of_first_appearance() {
    let out = ok(&[
        "groupby",
        "region",
        "sum(amount) as total, count() as n, mean(amount), max(product)",
        "sales.csv",
    ])
    .await;
    assert_eq!(
        out,
        "region,total,n,mean(amount),max(product)\nnorth,165.5,2,82.75,pear\nsouth,280,2,140,pear\neast,0,1,,apple\n"
    );
    let two = ok(&["groupby", "region,status", "count()", "sales.csv"]).await;
    assert!(
        two.starts_with("region,status,count()\nnorth,paid,2\nsouth,open,1\n"),
        "{two}"
    );
    // An expression over aggregations, and count(x) counting where x is true.
    let ratio = ok(&[
        "agg",
        "sum(amount) / count() as avg, count(amount > 100) as big, cardinality(region)",
        "sales.csv",
    ])
    .await;
    assert_eq!(ratio, "avg,big,cardinality(region)\n89.1,2,3\n");
    let r = xan(&["groupby", "region", "amount", "sales.csv"], None).await;
    assert!(r.stderr.contains("outside an aggregation"), "{}", r.stderr);
}

#[tokio::test]
async fn sums_of_money_come_out_as_the_sum() {
    let out = ok_stdin(&["agg", "sum(x)"], "x\n0.1\n0.2\n").await;
    assert_eq!(out, "sum(x)\n0.3\n");
}

#[tokio::test]
async fn stats_says_each_columns_type_and_numbers() {
    let out = ok(&["stats", "-s", "amount,region", "sales.csv"]).await;
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some(
            "field,count,count_empty,type,sum,mean,median,stddev,min,max,lex_first,lex_last,min_length,max_length"
        )
    );
    let amount: Vec<&str> = lines.next().unwrap().split(',').collect();
    assert_eq!(&amount[..4], ["amount", "5", "0", "float|string"]);
    assert_eq!(&amount[4..7], ["445.5", "111.375", "100"]);
    assert_eq!(&amount[8..10], ["45.5", "200"]);
    let region: Vec<&str> = lines.next().unwrap().split(',').collect();
    assert_eq!(
        region,
        [
            "region", "5", "0", "string", "", "", "", "", "", "", "east", "south", "4", "5"
        ]
    );
}

#[tokio::test]
async fn frequency_ranks_values_most_first_per_column() {
    let out = ok(&["frequency", "-s", "product,note", "sales.csv"]).await;
    assert_eq!(
        out,
        "field,value,count\nproduct,apple,3\nproduct,pear,2\nnote,<empty>,3\nnote,late, again,1\nnote,missing,1\n"
            .replace("late, again", "\"late, again\"")
    );
    assert_eq!(
        ok(&["frequency", "-s", "region", "-l", "1", "sales.csv"]).await,
        "field,value,count\nregion,north,2\n"
    );
}

#[tokio::test]
async fn it_reads_the_pipe_tabs_semicolons_and_headerless_files() {
    assert_eq!(ok_stdin(&["count"], "a,b\n1,2\n3,4\n").await, "2\n");
    assert_eq!(
        ok_stdin(&["select", "b", "-"], "a;b\n1;2\n").await,
        "b\n2\n"
    );
    assert_eq!(ok(&["select", "b", "t.tsv"]).await, "b\n2\n");
    assert_eq!(
        ok_stdin(&["select", "1", "-n"], "x,y\n1,2\n").await,
        "y\n2\n"
    );
    assert_eq!(
        ok_stdin(&["filter", "-n", "col(0) > 1"], "1,a\n2,b\n").await,
        "2,b\n"
    );
    // A quoted cell may hold a line break.
    assert_eq!(ok_stdin(&["count"], "a,b\n\"one\ntwo\",2\n").await, "1\n");
}

#[tokio::test]
async fn the_command_line_is_checked_and_help_is_there() {
    assert!(ok(&["--help"]).await.contains("groupby"));
    assert!(ok(&["groupby", "--help"]).await.contains("Aggregations"));
    let r = xan(&["pivot", "sales.csv"], None).await;
    assert_eq!(r.exit_code, 2);
    assert!(
        r.stderr.contains("no verb `pivot`") && r.stderr.contains("frequency"),
        "{}",
        r.stderr
    );
    let r = xan(&["filter", "sales.csv"], None).await;
    assert!(r.stderr.contains("no input"), "{}", r.stderr);
    let r = xan(&["filter", "amount", ">", "5", "sales.csv"], None).await;
    assert!(r.stderr.contains("quote an expression"), "{}", r.stderr);
    let r = xan(&["sort", "-x", "sales.csv"], None).await;
    assert!(r.stderr.contains("no option -x"), "{}", r.stderr);
    let r = xan(&["count", "nope.csv"], None).await;
    assert!(r.stderr.contains("nope.csv: no such file"), "{}", r.stderr);
}

#[tokio::test]
async fn it_stops_when_the_shell_calls_time_is_up() {
    let big: String = std::iter::once("n\n".to_string())
        .chain((0..5000).map(|i| format!("{i}\n")))
        .collect();
    let mut c = call(&["count"], Some(&big));
    c.ends_at = Instant::now();
    let r = Xan.run(c).await;
    assert_eq!(r.exit_code, 1);
    assert!(r.stderr.contains("ran out of time"), "{}", r.stderr);
}
