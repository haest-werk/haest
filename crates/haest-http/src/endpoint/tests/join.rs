//! `join_path`: where a path goes under a base URL, and that the check's
//! verdict on the base holds for the joined URL.

use super::*;

#[test]
fn a_path_goes_after_the_bases_path_and_before_its_query_and_fragment() {
    let cases = [
        ("https://gw.example/v1", "https://gw.example/v1/x/y"),
        ("https://gw.example/v1/", "https://gw.example/v1/x/y"),
        ("https://gw.example/v1//", "https://gw.example/v1/x/y"),
        ("https://gw.example", "https://gw.example/x/y"),
        (
            "https://gw.example/v1?key=k",
            "https://gw.example/v1/x/y?key=k",
        ),
        (
            "https://gw.example/v1/?key=k&v=2",
            "https://gw.example/v1/x/y?key=k&v=2",
        ),
        ("https://gw.example?key=k", "https://gw.example/x/y?key=k"),
        ("https://gw.example/v1#f", "https://gw.example/v1/x/y#f"),
        (
            "https://gw.example/v1?key=a/b#f/g",
            "https://gw.example/v1/x/y?key=a/b#f/g",
        ),
        (
            "https://u:p@ss@gw.example:8443/v1?key=k",
            "https://u:p@ss@gw.example:8443/v1/x/y?key=k",
        ),
    ];
    for (base, joined) in cases {
        assert_eq!(join_path(base, "x/y"), joined, "{base}");
    }
}

/// The check's verdict on a base URL holds for every path under it, so
/// checking the base or the URL sent is the same.
#[test]
fn a_path_under_a_url_keeps_the_checks_verdict() {
    for url in MISREAD {
        let joined = join_path(url, "v1/messages");
        assert!(check_endpoint_url(&joined).is_err(), "{joined}");
    }
    for url in READ_RIGHT {
        let joined = join_path(url, "v1/messages");
        assert_eq!(check_endpoint_url(&joined), Ok(()), "{joined}");
    }
}
