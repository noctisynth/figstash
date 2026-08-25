//! Deterministic, pure-local snapshot comparison.

use figstash_core::{
    AppResult, IndexedEntity, SnapshotDiffNode, SnapshotRepository, SnapshotSummary,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Filters applied to node diff categories.
#[derive(Debug, Clone, Copy, Default)]
pub struct SnapshotDiffFilters<'a> {
    /// Restrict output to this node subtree in either snapshot.
    pub node_id: Option<&'a str>,
    /// Restrict output to this exact Figma node type in either snapshot.
    pub node_type: Option<&'a str>,
    /// Restrict output to this root-to-node identifier prefix in either snapshot.
    pub path_prefix: Option<&'a [String]>,
}

/// Machine-readable result of comparing two immutable snapshots.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDiffData {
    /// Before snapshot.
    pub snapshot_a: SnapshotSummary,
    /// After snapshot.
    pub snapshot_b: SnapshotSummary,
    /// Counts for every output category.
    pub summary: SnapshotDiffSummary,
    /// Node-level changes.
    pub nodes: NodeChanges,
    /// Style and component-map changes.
    pub entities: EntityChangesByKind,
}

/// Counts for node and entity changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDiffSummary {
    /// Nodes only present in the after snapshot.
    pub added: usize,
    /// Nodes only present in the before snapshot.
    pub removed: usize,
    /// Retained nodes whose own fields changed.
    pub changed: usize,
    /// Retained nodes whose direct parent or sibling order changed.
    pub moved: usize,
    /// Retained nodes changed only below themselves.
    pub descendant_only: usize,
    /// Style-map counts.
    pub styles: EntityDiffCounts,
    /// Component-map counts.
    pub components: EntityDiffCounts,
    /// Component-set-map counts.
    pub component_sets: EntityDiffCounts,
}

/// Counts for one entity kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDiffCounts {
    /// Entities only present in the after snapshot.
    pub added: usize,
    /// Entities only present in the before snapshot.
    pub removed: usize,
    /// Retained entities whose indexed value changed.
    pub changed: usize,
}

/// Stable node state included in diff output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffNodeState {
    /// Stable Figma node identifier.
    pub node_id: String,
    /// Human-readable node name.
    pub name: String,
    /// Figma node type.
    pub node_type: String,
    /// Direct parent identifier.
    pub parent_id: Option<String>,
    /// Zero-based order among siblings.
    pub sibling_order: u32,
    /// Root-to-node identifier path.
    pub path_ids: Vec<String>,
    /// Deterministic normalized subtree hash.
    pub subtree_hash: String,
}

/// Before/after state for a retained node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffNodeChange {
    /// Stable Figma node identifier.
    pub node_id: String,
    /// State in the before snapshot.
    pub before: DiffNodeState,
    /// State in the after snapshot.
    pub after: DiffNodeState,
}

/// Categorized node changes. Changed and moved arrays may overlap by node ID.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeChanges {
    /// Nodes only present in the after snapshot.
    pub added: Vec<DiffNodeState>,
    /// Nodes only present in the before snapshot.
    pub removed: Vec<DiffNodeState>,
    /// Retained nodes whose own fields changed.
    pub changed: Vec<DiffNodeChange>,
    /// Retained nodes whose direct parent or sibling order changed.
    pub moved: Vec<DiffNodeChange>,
    /// Retained nodes changed only below themselves.
    pub descendant_only: Vec<DiffNodeChange>,
}

/// Before/after state for a retained style or component entity.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityChange {
    /// Stable entity identifier.
    pub id: String,
    /// Entity in the before snapshot.
    pub before: IndexedEntity,
    /// Entity in the after snapshot.
    pub after: IndexedEntity,
}

/// Categorized changes for one entity kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityDiff {
    /// Entities only present in the after snapshot.
    pub added: Vec<IndexedEntity>,
    /// Entities only present in the before snapshot.
    pub removed: Vec<IndexedEntity>,
    /// Retained entities whose indexed value changed.
    pub changed: Vec<EntityChange>,
}

/// Changes in all indexed top-level entity maps.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityChangesByKind {
    /// Indexed style changes.
    pub styles: EntityDiff,
    /// Indexed component changes.
    pub components: EntityDiff,
    /// Indexed component-set changes.
    pub component_sets: EntityDiff,
}

pub(crate) async fn compare_snapshots<R: SnapshotRepository>(
    repository: &R,
    snapshot_a: SnapshotSummary,
    snapshot_b: SnapshotSummary,
    filters: SnapshotDiffFilters<'_>,
) -> AppResult<SnapshotDiffData> {
    let before_nodes = repository.load_diff_nodes(&snapshot_a.id).await?;
    let after_nodes = repository.load_diff_nodes(&snapshot_b.id).await?;
    let nodes = compare_nodes(before_nodes, after_nodes, filters);
    let entities = EntityChangesByKind {
        styles: compare_entities(
            repository.load_styles(&snapshot_a.id).await?,
            repository.load_styles(&snapshot_b.id).await?,
        ),
        components: compare_entities(
            repository.load_components(&snapshot_a.id).await?,
            repository.load_components(&snapshot_b.id).await?,
        ),
        component_sets: compare_entities(
            repository.load_component_sets(&snapshot_a.id).await?,
            repository.load_component_sets(&snapshot_b.id).await?,
        ),
    };
    let summary = SnapshotDiffSummary {
        added: nodes.added.len(),
        removed: nodes.removed.len(),
        changed: nodes.changed.len(),
        moved: nodes.moved.len(),
        descendant_only: nodes.descendant_only.len(),
        styles: entity_counts(&entities.styles),
        components: entity_counts(&entities.components),
        component_sets: entity_counts(&entities.component_sets),
    };
    Ok(SnapshotDiffData {
        snapshot_a,
        snapshot_b,
        summary,
        nodes,
        entities,
    })
}

fn compare_nodes(
    before: Vec<SnapshotDiffNode>,
    after: Vec<SnapshotDiffNode>,
    filters: SnapshotDiffFilters<'_>,
) -> NodeChanges {
    let before = before
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .into_iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut ids = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    let mut changes = NodeChanges::default();
    for id in ids {
        let before_node = before.get(&id);
        let after_node = after.get(&id);
        if !matches_filters(before_node, after_node, filters) {
            continue;
        }
        match (before_node, after_node) {
            (None, Some(node)) => changes.added.push(node_state(node)),
            (Some(node), None) => changes.removed.push(node_state(node)),
            (Some(before_node), Some(after_node)) => {
                let own_changed = before_node.own_hash != after_node.own_hash;
                let moved = before_node.parent_id != after_node.parent_id
                    || before_node.sibling_order != after_node.sibling_order;
                let change = || DiffNodeChange {
                    node_id: id.clone(),
                    before: node_state(before_node),
                    after: node_state(after_node),
                };
                if own_changed {
                    changes.changed.push(change());
                }
                if moved {
                    changes.moved.push(change());
                }
                if !own_changed && !moved && before_node.subtree_hash != after_node.subtree_hash {
                    changes.descendant_only.push(change());
                }
            }
            (None, None) => {}
        }
    }
    changes
}

fn matches_filters(
    before: Option<&SnapshotDiffNode>,
    after: Option<&SnapshotDiffNode>,
    filters: SnapshotDiffFilters<'_>,
) -> bool {
    [before, after].into_iter().flatten().any(|node| {
        filters
            .node_id
            .is_none_or(|node_id| node.path_ids.iter().any(|id| id == node_id))
            && filters
                .node_type
                .is_none_or(|node_type| node.node_type == node_type)
            && filters
                .path_prefix
                .is_none_or(|prefix| node.path_ids.starts_with(prefix))
    })
}

fn node_state(node: &SnapshotDiffNode) -> DiffNodeState {
    DiffNodeState {
        node_id: node.node_id.clone(),
        name: node.name.clone(),
        node_type: node.node_type.clone(),
        parent_id: node.parent_id.clone(),
        sibling_order: node.sibling_order,
        path_ids: node.path_ids.clone(),
        subtree_hash: node.subtree_hash.clone(),
    }
}

fn compare_entities(before: Vec<IndexedEntity>, after: Vec<IndexedEntity>) -> EntityDiff {
    let before = before
        .into_iter()
        .map(|entity| (entity.id.clone(), entity))
        .collect::<BTreeMap<_, _>>();
    let after = after
        .into_iter()
        .map(|entity| (entity.id.clone(), entity))
        .collect::<BTreeMap<_, _>>();
    let mut ids = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    let mut diff = EntityDiff::default();
    for id in ids {
        match (before.get(&id), after.get(&id)) {
            (None, Some(entity)) => diff.added.push(entity.clone()),
            (Some(entity), None) => diff.removed.push(entity.clone()),
            (Some(before), Some(after)) if before != after => diff.changed.push(EntityChange {
                id,
                before: before.clone(),
                after: after.clone(),
            }),
            _ => {}
        }
    }
    diff
}

fn entity_counts(diff: &EntityDiff) -> EntityDiffCounts {
    EntityDiffCounts {
        added: diff.added.len(),
        removed: diff.removed.len(),
        changed: diff.changed.len(),
    }
}
