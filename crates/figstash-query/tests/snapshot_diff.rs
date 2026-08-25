//! Offline snapshot diff acceptance coverage.

use figstash_core::{ErrorCode, GeometryMode, RequestProfile, SnapshotSelector};
use figstash_query::{QueryService, SnapshotDiffFilters, parse_file};
use figstash_store::Store;
use std::fs;

const FILE_KEY: &str = "SnapshotDiffFixture";

async fn commit_fixture(
    store: &Store,
    bytes: &[u8],
    profile: RequestProfile,
) -> figstash_core::SnapshotSummary {
    let indexed = parse_file(bytes).unwrap_or_else(|error| panic!("fixture parse failed: {error}"));
    let stage = store
        .create_staging_file()
        .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
    fs::write(&stage, bytes).unwrap_or_else(|error| panic!("fixture staging failed: {error}"));
    store
        .commit_snapshot(FILE_KEY, profile.key(), &stage, &indexed)
        .await
        .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"))
}

async fn prepared_diff() -> (
    tempfile::TempDir,
    Store,
    figstash_core::SnapshotSummary,
    figstash_core::SnapshotSummary,
) {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let store = Store::open(temporary.path())
        .await
        .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
    let before = commit_fixture(
        &store,
        include_bytes!("../../../fixtures/figma/snapshot-diff-before.json"),
        RequestProfile::default(),
    )
    .await;
    let after = commit_fixture(
        &store,
        include_bytes!("../../../fixtures/figma/snapshot-diff-after.json"),
        RequestProfile::default(),
    )
    .await;
    (temporary, store, before, after)
}

fn selector<'a>(snapshot_id: &'a str, profile: &'a str) -> SnapshotSelector<'a> {
    SnapshotSelector {
        file_key: FILE_KEY,
        request_profile: profile,
        snapshot_id: Some(snapshot_id),
    }
}

#[tokio::test]
async fn classifies_nodes_and_entities_without_descendant_false_positives() {
    let (_temporary, store, before, after) = prepared_diff().await;
    let service = QueryService::new(store);
    let result = service
        .snapshot_diff(
            selector(&before.id, RequestProfile::default().key()),
            selector(&after.id, RequestProfile::default().key()),
            SnapshotDiffFilters::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("snapshot diff failed: {error}"));

    assert_eq!(result.summary.added, 1);
    assert_eq!(result.summary.removed, 1);
    assert_eq!(result.summary.changed, 2);
    assert_eq!(result.summary.moved, 1);
    assert_eq!(result.summary.descendant_only, 4);
    assert_eq!(result.nodes.added[0].node_id, "7:1");
    assert_eq!(result.nodes.removed[0].node_id, "4:1");
    assert_eq!(
        result
            .nodes
            .changed
            .iter()
            .map(|change| change.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["3:1", "5:1"]
    );
    assert_eq!(result.nodes.moved[0].node_id, "5:1");
    assert_eq!(
        result
            .nodes
            .descendant_only
            .iter()
            .map(|change| change.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["0:0", "1:1", "1:2", "2:1"]
    );
    assert_eq!(result.summary.styles.added, 1);
    assert_eq!(result.summary.styles.changed, 1);
    assert_eq!(result.summary.components.added, 1);
    assert_eq!(result.summary.components.changed, 1);
    assert_eq!(result.summary.component_sets.changed, 1);
}

#[tokio::test]
async fn combines_node_type_and_path_filters_deterministically() {
    let (_temporary, store, before, after) = prepared_diff().await;
    let service = QueryService::new(store);
    let path = vec!["0:0".to_owned(), "1:2".to_owned()];
    let filters = SnapshotDiffFilters {
        node_id: Some("1:2"),
        node_type: Some("FRAME"),
        path_prefix: Some(&path),
    };
    let first = service
        .snapshot_diff(
            selector(&before.id, RequestProfile::default().key()),
            selector(&after.id, RequestProfile::default().key()),
            filters,
        )
        .await
        .unwrap_or_else(|error| panic!("filtered snapshot diff failed: {error}"));
    let second = service
        .snapshot_diff(
            selector(&before.id, RequestProfile::default().key()),
            selector(&after.id, RequestProfile::default().key()),
            filters,
        )
        .await
        .unwrap_or_else(|error| panic!("repeated snapshot diff failed: {error}"));

    assert_eq!(first, second);
    assert_eq!(first.nodes.changed.len(), 1);
    assert_eq!(first.nodes.changed[0].node_id, "5:1");
    assert_eq!(first.nodes.moved.len(), 1);
    assert_eq!(first.nodes.moved[0].node_id, "5:1");
    assert_eq!(first.summary.added, 0);
    assert_eq!(first.summary.removed, 0);
}

#[tokio::test]
async fn rejects_snapshots_from_different_request_profiles() {
    let (_temporary, store, before, _after) = prepared_diff().await;
    let paths_profile = RequestProfile {
        geometry: GeometryMode::Paths,
    };
    let paths = commit_fixture(
        &store,
        include_bytes!("../../../fixtures/figma/snapshot-diff-after.json"),
        paths_profile,
    )
    .await;
    let service = QueryService::new(store);
    let result = service
        .snapshot_diff(
            selector(&before.id, RequestProfile::default().key()),
            selector(&paths.id, paths_profile.key()),
            SnapshotDiffFilters::default(),
        )
        .await;
    let Err(error) = result else {
        panic!("cross-profile snapshot diff must fail");
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
}
