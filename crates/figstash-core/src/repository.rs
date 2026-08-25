//! Read-only repository boundary used by pure-local query code.

use crate::{
    ApiAttempt, AppResult, ComponentUsage, IndexedEntity, NodeSearchQuery, NodeSearchResult,
    SnapshotDiffNode, SnapshotSelector, SnapshotSummary, StoredNode,
};

/// Durable sink for locally observed Figma request attempts.
#[allow(async_fn_in_trait)]
pub trait AttemptRecorder: Send + Sync {
    /// Persists one completed request attempt without storing credentials or payloads.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the attempt cannot be persisted durably.
    async fn record_attempt(&self, attempt: &ApiAttempt) -> AppResult<()>;
}

/// Pure-local snapshot query interface.
///
/// The trait intentionally exposes no transport or network capability. Query
/// implementations can therefore be audited structurally for offline safety.
#[allow(async_fn_in_trait)]
pub trait SnapshotRepository {
    /// Resolves an explicit snapshot or the current file/profile head.
    ///
    /// # Errors
    ///
    /// Returns a local-data or storage error when the snapshot cannot be resolved.
    async fn resolve_snapshot(&self, selector: SnapshotSelector<'_>) -> AppResult<SnapshotSummary>;

    /// Loads a node and its ordered direct children.
    ///
    /// # Errors
    ///
    /// Returns `node_not_found` or a durable storage error.
    async fn load_node(&self, snapshot_id: &str, node_id: &str) -> AppResult<StoredNode>;

    /// Loads every indexed node in deterministic preorder.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when indexed nodes cannot be loaded.
    async fn load_diff_nodes(&self, snapshot_id: &str) -> AppResult<Vec<SnapshotDiffNode>>;

    /// Returns the document root node identifier.
    ///
    /// # Errors
    ///
    /// Returns a corruption or storage error when no root can be loaded.
    async fn root_node_id(&self, snapshot_id: &str) -> AppResult<String>;

    /// Resolves an exact identifier or name to candidate nodes.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the local index cannot be queried.
    async fn node_candidates(
        &self,
        snapshot_id: &str,
        identifier_or_name: &str,
    ) -> AppResult<Vec<NodeSearchResult>>;

    /// Searches the local node index and returns an opaque next cursor.
    ///
    /// # Errors
    ///
    /// Returns an input error for an invalid cursor or a local storage error.
    async fn search_nodes(
        &self,
        snapshot_id: &str,
        query: &NodeSearchQuery,
    ) -> AppResult<(Vec<NodeSearchResult>, Option<String>)>;

    /// Loads indexed styles.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when indexed styles cannot be loaded.
    async fn load_styles(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>>;

    /// Loads indexed components.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when components cannot be loaded.
    async fn load_components(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>>;

    /// Loads indexed component sets.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when component sets cannot be loaded.
    async fn load_component_sets(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>>;

    /// Lists instances referencing a component.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the local relation index cannot be queried.
    async fn component_usage(
        &self,
        snapshot_id: &str,
        component_id: &str,
    ) -> AppResult<ComponentUsage>;
}
