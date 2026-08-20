//! End-to-end fixture coverage for the structurally offline query service.

use figstash_core::{NodeSearchQuery, RequestProfile, SnapshotSelector};
use figstash_query::{NodeGetOptions, QueryService, View, parse_file};
use figstash_store::Store;
use serde_json::json;
use std::fs;

fn prepared_store() -> (tempfile::TempDir, Store, figstash_core::SnapshotSummary) {
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let store = Store::open(temporary.path())
        .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
    let bytes = include_bytes!("../../../fixtures/figma/feature-parity.json");
    let indexed = parse_file(bytes).unwrap_or_else(|error| panic!("fixture parse failed: {error}"));
    let stage = store
        .create_staging_file()
        .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
    fs::write(&stage, bytes).unwrap_or_else(|error| panic!("fixture staging failed: {error}"));
    let summary = store
        .commit_snapshot(
            "SyntheticFileKey123",
            RequestProfile::default().key(),
            &stage,
            &indexed,
        )
        .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"));
    (temporary, store, summary)
}

#[test]
fn compact_raw_search_tokens_and_components_are_fully_offline() {
    let (_temporary, store, summary) = prepared_store();
    let service = QueryService::new(store);
    let selector = || SnapshotSelector {
        file_key: "SyntheticFileKey123",
        request_profile: RequestProfile::default().key(),
        snapshot_id: Some(&summary.id),
    };

    let compact = service
        .node_get(NodeGetOptions {
            selector: selector(),
            node_id: Some("2:1"),
            depth: Some(1),
            view: View::Compact,
        })
        .unwrap_or_else(|error| panic!("compact query failed: {error}"));
    insta::assert_json_snapshot!("compact_card", compact.node);
    let references = compact
        .references
        .unwrap_or_else(|| panic!("compact output must include referenced design context"));
    assert_eq!(
        references
            .named_styles
            .iter()
            .map(|style| style.id.as_str())
            .collect::<Vec<_>>(),
        ["style-effect-1", "style-fill-1", "style-text-1"]
    );
    assert!(!references.global_vars.is_empty());
    insta::assert_json_snapshot!("d2c_card_references", references);

    let instance = service
        .node_get(NodeGetOptions {
            selector: selector(),
            node_id: Some("4:1"),
            depth: None,
            view: View::Compact,
        })
        .unwrap_or_else(|error| panic!("instance query failed: {error}"));
    let instance_references = instance
        .references
        .unwrap_or_else(|| panic!("instance output must include component context"));
    assert_eq!(instance_references.components.len(), 1);
    assert_eq!(instance_references.components[0].id, "3:1");

    let raw = service
        .node_get(NodeGetOptions {
            selector: selector(),
            node_id: Some("5:1"),
            depth: None,
            view: View::Raw,
        })
        .unwrap_or_else(|error| panic!("raw query failed: {error}"));
    assert_eq!(raw.node["futurePayload"]["preserved"], true);
    assert!(raw.references.is_none());

    let search = service
        .node_search(
            selector(),
            &NodeSearchQuery {
                text: Some("Hello".to_owned()),
                limit: 10,
                ..NodeSearchQuery::default()
            },
        )
        .unwrap_or_else(|error| panic!("text search failed: {error}"));
    assert_eq!(search.nodes.len(), 1);
    assert_eq!(search.nodes[0].node_id, "2:2");

    let tokens = service
        .tokens_get(selector())
        .unwrap_or_else(|error| panic!("token query failed: {error}"));
    assert_eq!(tokens.named_styles.len(), 3);
    assert!(!tokens.global_vars.is_empty());
    insta::assert_json_snapshot!("derived_global_vars", tokens.global_vars);

    let components = service
        .components_list(selector())
        .unwrap_or_else(|error| panic!("component query failed: {error}"));
    assert_eq!(components.components.len(), 1);
    assert_eq!(components.component_sets.len(), 1);
    assert_eq!(components.usage["3:1"].instance_node_ids, ["4:1"]);
}

#[test]
fn all_supported_core_node_types_have_compact_golden_output() {
    let node_types = [
        "CANVAS",
        "FRAME",
        "GROUP",
        "SECTION",
        "VECTOR",
        "BOOLEAN_OPERATION",
        "STAR",
        "LINE",
        "ELLIPSE",
        "REGULAR_POLYGON",
        "RECTANGLE",
        "TEXT",
        "SLICE",
        "COMPONENT",
        "COMPONENT_SET",
        "INSTANCE",
        "STAMP",
        "WIDGET",
        "EMBED",
        "LINK_UNFURL",
        "MEDIA",
        "SHAPE_WITH_TEXT",
        "CODE_BLOCK",
        "CONNECTOR",
        "TABLE",
        "TABLE_CELL",
        "HIGHLIGHT",
        "WASHI_TAPE",
    ];
    let children = node_types
        .iter()
        .enumerate()
        .map(|(index, node_type)| {
            json!({
                "id": format!("1:{}", index + 1),
                "name": node_type,
                "type": node_type,
                "visible": true
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&json!({
        "name": "Core Node Types",
        "version": "1",
        "document": {
            "id": "0:0",
            "name": "Document",
            "type": "DOCUMENT",
            "children": children
        }
    }))
    .unwrap_or_else(|error| panic!("fixture serialization failed: {error}"));
    let indexed =
        parse_file(&bytes).unwrap_or_else(|error| panic!("fixture parse failed: {error}"));
    assert!(indexed.warnings.is_empty());
    let temporary =
        tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
    let store = Store::open(temporary.path())
        .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
    let stage = store
        .create_staging_file()
        .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
    fs::write(&stage, &bytes).unwrap_or_else(|error| panic!("fixture staging failed: {error}"));
    let summary = store
        .commit_snapshot(
            "CoreNodeTypesKey",
            RequestProfile::default().key(),
            &stage,
            &indexed,
        )
        .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"));
    let compact = QueryService::new(store)
        .node_get(NodeGetOptions {
            selector: SnapshotSelector {
                file_key: "CoreNodeTypesKey",
                request_profile: RequestProfile::default().key(),
                snapshot_id: Some(&summary.id),
            },
            node_id: None,
            depth: Some(1),
            view: View::Compact,
        })
        .unwrap_or_else(|error| panic!("compact query failed: {error}"));
    insta::assert_json_snapshot!("compact_core_node_types", compact.node);
}
