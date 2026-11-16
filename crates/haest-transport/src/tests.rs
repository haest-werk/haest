//! The errors a connection meets, as a host shows them.

use std::error::Error as _;

use haest_core::testing::assert_causes_said_once;
use haest_wire::{DecodeError, decode};

use crate::TransportError;

/// Each error with a cause says its own words; the chain says the cause,
/// once.
#[test]
fn every_error_with_a_cause_says_it_once() {
    let denied = || std::io::Error::other("permission denied");
    let not_json = || serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    assert_causes_said_once(&TransportError::Path {
        path: "/run/x.sock".into(),
        source: denied(),
    });
    assert_causes_said_once(&TransportError::NotAFrame(not_json()));
    assert_causes_said_once(&DecodeError::Json(not_json()));
    let shape = decode(r#"{"v": 1, "id": "x", "type": "nope"}"#).unwrap_err();
    assert!(
        shape.source().is_some(),
        "serde's reason is the cause: {shape:?}"
    );
    assert_causes_said_once(&shape);
}
