//! Smoke test for the workspace-wide `insta` fixture conventions.

use std::collections::BTreeMap;

#[test]
fn fixture_conventions_are_snapshotted() {
    let conventions = BTreeMap::from([
        ("inputs", "fixtures/figma"),
        ("outputs", "tests/snapshots"),
        ("updates", "explicit-review"),
    ]);

    insta::assert_json_snapshot!("fixture_conventions", conventions);
}
