//! The verbs that sum rows up: groupby and agg, stats, frequency.

use super::{ok, ok_stdin, xan};

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
    // A bare column in a groupby list fails and names the way out: first(it).
    let r = xan(&["groupby", "region", "amount", "sales.csv"], None).await;
    assert_eq!(r.exit_code, 1, "{}", r.stderr);
    assert!(r.stderr.contains("first(amount)"), "{}", r.stderr);
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
