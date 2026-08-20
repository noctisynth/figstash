//! Pure-local application service for cached Figma data.

use crate::transform::{CompactNode, DerivedVariable, compact_node, derive_variables, raw_node};
use figstash_core::{
    AppError, AppResult, ComponentUsage, ErrorCode, IndexedEntity, NodeSearchQuery,
    NodeSearchResult, SnapshotRepository, SnapshotSelector, SnapshotSummary, StoredNode,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Node rendering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Agent-oriented normalized context.
    Compact,
    /// Reconstructed lossless Figma node JSON.
    Raw,
}

/// Options for one local node lookup.
#[derive(Debug, Clone, Copy)]
pub struct NodeGetOptions<'a> {
    /// Snapshot selection.
    pub selector: SnapshotSelector<'a>,
    /// Exact node identifier, or `None` for the document root.
    pub node_id: Option<&'a str>,
    /// Maximum descendant depth, or `None` for the entire subtree.
    pub depth: Option<u32>,
    /// Output representation.
    pub view: View,
}

/// Data returned by `node get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeGetData {
    /// Resolved immutable snapshot.
    pub snapshot: SnapshotSummary,
    /// Compact or raw node tree.
    pub node: Value,
    /// Referenced design-system context for compact output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<NodeReferences>,
}

/// Design-system entities referenced by a compact node subtree.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeReferences {
    /// Named styles referenced by nodes in the returned subtree.
    pub named_styles: Vec<IndexedEntity>,
    /// Repeated values referenced by nodes in the returned subtree.
    pub global_vars: Vec<DerivedVariable>,
    /// Component metadata referenced by instances or definitions in the subtree.
    pub components: Vec<IndexedEntity>,
    /// Component-set metadata whose definition nodes are in the subtree.
    pub component_sets: Vec<IndexedEntity>,
}

/// Data returned by `node search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchData {
    /// Resolved immutable snapshot.
    pub snapshot: SnapshotSummary,
    /// Matching local nodes.
    pub nodes: Vec<NodeSearchResult>,
    /// Opaque next page cursor.
    pub next_cursor: Option<String>,
}

/// Data returned by `tokens get`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokensData {
    /// Resolved immutable snapshot.
    pub snapshot: SnapshotSummary,
    /// Named styles supplied by Figma.
    pub named_styles: Vec<IndexedEntity>,
    /// Repeated values derived locally; these are not Figma Variables API data.
    pub global_vars: Vec<DerivedVariable>,
    /// Explicit provenance marker for the derived values.
    pub global_vars_source: String,
}

/// Data returned by `components list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentsData {
    /// Resolved immutable snapshot.
    pub snapshot: SnapshotSummary,
    /// Figma components indexed from the file response.
    pub components: Vec<IndexedEntity>,
    /// Figma component sets indexed from the file response.
    pub component_sets: Vec<IndexedEntity>,
    /// Instance usage keyed by component identifier.
    pub usage: BTreeMap<String, ComponentUsage>,
}

/// Offline query service parameterized only by a local repository.
pub struct QueryService<R> {
    repository: R,
}

impl<R> QueryService<R>
where
    R: SnapshotRepository,
{
    /// Creates a local query service. No network dependency can be supplied.
    pub const fn new(repository: R) -> Self {
        Self { repository }
    }

    /// Returns one reconstructed node subtree.
    ///
    /// # Errors
    ///
    /// Returns local-data, integrity, or serialization errors; it never accesses a network.
    pub fn node_get(&self, options: NodeGetOptions<'_>) -> AppResult<NodeGetData> {
        let snapshot = self.repository.resolve_snapshot(options.selector)?;
        let node_id = match options.node_id {
            Some(node_id) => node_id.to_owned(),
            None => self.repository.root_node_id(&snapshot.id)?,
        };
        let (node, references) = match options.view {
            View::Compact => {
                let mut selected_nodes = Vec::new();
                let node = self.load_compact(
                    &snapshot.id,
                    &node_id,
                    options.depth,
                    0,
                    &mut selected_nodes,
                )?;
                (
                    serde_json::to_value(node).map_err(|error| serialization_error(&error))?,
                    Some(self.node_references(&snapshot.id, &selected_nodes)?),
                )
            }
            View::Raw => (
                self.load_raw(&snapshot.id, &node_id, options.depth, 0)?,
                None,
            ),
        };
        Ok(NodeGetData {
            snapshot,
            node,
            references,
        })
    }

    /// Searches one immutable snapshot without any cache-miss fallback.
    ///
    /// # Errors
    ///
    /// Returns input, local-data, or storage errors from the local repository.
    pub fn node_search(
        &self,
        selector: SnapshotSelector<'_>,
        query: &NodeSearchQuery,
    ) -> AppResult<SearchData> {
        let snapshot = self.repository.resolve_snapshot(selector)?;
        let (nodes, next_cursor) = self.repository.search_nodes(&snapshot.id, query)?;
        Ok(SearchData {
            snapshot,
            nodes,
            next_cursor,
        })
    }

    /// Returns named styles and deterministic locally derived repeated values.
    ///
    /// # Errors
    ///
    /// Returns local-data, integrity, or deterministic transformation errors.
    pub fn tokens_get(&self, selector: SnapshotSelector<'_>) -> AppResult<TokensData> {
        let snapshot = self.repository.resolve_snapshot(selector)?;
        let root_id = self.repository.root_node_id(&snapshot.id)?;
        let mut nodes = Vec::new();
        self.collect_nodes(&snapshot.id, &root_id, &mut nodes)?;
        Ok(TokensData {
            named_styles: self.repository.load_styles(&snapshot.id)?,
            global_vars: derive_variables(&nodes)?,
            global_vars_source: "derived_from_file_content".to_owned(),
            snapshot,
        })
    }

    /// Returns components, component sets, and local instance usage.
    ///
    /// # Errors
    ///
    /// Returns local-data, integrity, or storage errors from the repository.
    pub fn components_list(&self, selector: SnapshotSelector<'_>) -> AppResult<ComponentsData> {
        let snapshot = self.repository.resolve_snapshot(selector)?;
        let components = self.repository.load_components(&snapshot.id)?;
        let component_sets = self.repository.load_component_sets(&snapshot.id)?;
        let mut usage = BTreeMap::new();
        for component in &components {
            usage.insert(
                component.id.clone(),
                self.repository
                    .component_usage(&snapshot.id, &component.id)?,
            );
        }
        Ok(ComponentsData {
            snapshot,
            components,
            component_sets,
            usage,
        })
    }

    fn load_compact(
        &self,
        snapshot_id: &str,
        node_id: &str,
        max_depth: Option<u32>,
        current_depth: u32,
        selected_nodes: &mut Vec<StoredNode>,
    ) -> AppResult<CompactNode> {
        let node = self.load_node_with_candidates(snapshot_id, node_id)?;
        selected_nodes.push(node.clone());
        let mut children = Vec::new();
        if max_depth.is_none_or(|maximum| current_depth < maximum) {
            for child_id in &node.child_ids {
                children.push(self.load_compact(
                    snapshot_id,
                    child_id,
                    max_depth,
                    current_depth + 1,
                    selected_nodes,
                )?);
            }
        }
        Ok(compact_node(&node, children))
    }

    fn node_references(
        &self,
        snapshot_id: &str,
        selected_nodes: &[StoredNode],
    ) -> AppResult<NodeReferences> {
        let selected_ids = selected_nodes
            .iter()
            .map(|node| node.index.node_id.as_str())
            .collect::<BTreeSet<_>>();
        let style_ids = selected_nodes
            .iter()
            .flat_map(style_references)
            .collect::<BTreeSet<_>>();
        let component_ids = selected_nodes
            .iter()
            .filter_map(|node| node.index.component_id.as_deref())
            .collect::<BTreeSet<_>>();

        let named_styles = self
            .repository
            .load_styles(snapshot_id)?
            .into_iter()
            .filter(|style| style_ids.contains(style.id.as_str()))
            .collect();
        let components = self
            .repository
            .load_components(snapshot_id)?
            .into_iter()
            .filter(|component| {
                component_ids.contains(component.id.as_str())
                    || component
                        .node_id
                        .as_deref()
                        .is_some_and(|node_id| selected_ids.contains(node_id))
            })
            .collect();
        let component_sets = self
            .repository
            .load_component_sets(snapshot_id)?
            .into_iter()
            .filter(|component_set| {
                component_set
                    .node_id
                    .as_deref()
                    .is_some_and(|node_id| selected_ids.contains(node_id))
            })
            .collect();
        let global_vars = derive_variables(selected_nodes)?;

        Ok(NodeReferences {
            named_styles,
            global_vars,
            components,
            component_sets,
        })
    }

    fn load_raw(
        &self,
        snapshot_id: &str,
        node_id: &str,
        max_depth: Option<u32>,
        current_depth: u32,
    ) -> AppResult<Value> {
        let node = self.load_node_with_candidates(snapshot_id, node_id)?;
        let mut children = Vec::new();
        if max_depth.is_none_or(|maximum| current_depth < maximum) {
            for child_id in &node.child_ids {
                children.push(self.load_raw(
                    snapshot_id,
                    child_id,
                    max_depth,
                    current_depth + 1,
                )?);
            }
        }
        Ok(raw_node(&node, children))
    }

    fn collect_nodes(
        &self,
        snapshot_id: &str,
        node_id: &str,
        output: &mut Vec<StoredNode>,
    ) -> AppResult<()> {
        let node = self.repository.load_node(snapshot_id, node_id)?;
        let child_ids = node.child_ids.clone();
        output.push(node);
        for child_id in child_ids {
            self.collect_nodes(snapshot_id, &child_id, output)?;
        }
        Ok(())
    }

    fn load_node_with_candidates(&self, snapshot_id: &str, node_id: &str) -> AppResult<StoredNode> {
        match self.repository.load_node(snapshot_id, node_id) {
            Ok(node) => Ok(node),
            Err(error) if error.code() == ErrorCode::NodeNotFound => {
                let candidates = self.repository.node_candidates(snapshot_id, node_id)?;
                Err(error.with_details(json!({
                    "nodeId": node_id,
                    "candidates": candidates,
                })))
            }
            Err(error) => Err(error),
        }
    }
}

fn style_references(node: &StoredNode) -> Vec<&str> {
    node.index
        .raw
        .get("styles")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|styles| styles.values())
        .filter_map(Value::as_str)
        .collect()
}

fn serialization_error(error: &serde_json::Error) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "Failed to serialize local query output.",
    )
    .with_details(json!({"reason": error.to_string()}))
}
