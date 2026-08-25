//! Toasty models for the durable `SQLite` catalog.

#[derive(Debug, Clone, toasty::Model)]
#[table = "snapshots"]
pub(crate) struct SnapshotRow {
    #[key]
    pub(crate) id: String,
    pub(crate) file_key: String,
    pub(crate) figma_version: String,
    pub(crate) request_profile: String,
    pub(crate) file_name: String,
    pub(crate) fetched_at: String,
    pub(crate) last_modified: Option<String>,
    pub(crate) raw_blob_hash: String,
    pub(crate) raw_size: i64,
    pub(crate) node_count: i64,
    pub(crate) parser_version: i64,
    pub(crate) status: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "heads"]
pub(crate) struct HeadRow {
    #[key]
    pub(crate) file_key: String,
    #[key]
    pub(crate) request_profile: String,
    pub(crate) snapshot_id: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "nodes"]
pub(crate) struct NodeRow {
    #[key]
    pub(crate) snapshot_id: String,
    #[key]
    pub(crate) node_id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) node_type: String,
    pub(crate) name: String,
    pub(crate) depth: i64,
    pub(crate) sibling_order: i64,
    pub(crate) path_ids: String,
    pub(crate) visible: Option<bool>,
    pub(crate) x: Option<f64>,
    pub(crate) y: Option<f64>,
    pub(crate) width: Option<f64>,
    pub(crate) height: Option<f64>,
    pub(crate) component_id: Option<String>,
    pub(crate) text_content: Option<String>,
    pub(crate) node_blob_hash: String,
    pub(crate) subtree_hash: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "styles"]
pub(crate) struct StyleRow {
    #[key]
    pub(crate) snapshot_id: String,
    #[key]
    pub(crate) style_id: String,
    pub(crate) style_type: String,
    pub(crate) name: Option<String>,
    pub(crate) json_blob_hash: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "components"]
pub(crate) struct ComponentRow {
    #[key]
    pub(crate) snapshot_id: String,
    #[key]
    pub(crate) component_id: String,
    pub(crate) node_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) json_blob_hash: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "component_sets"]
pub(crate) struct ComponentSetRow {
    #[key]
    pub(crate) snapshot_id: String,
    #[key]
    pub(crate) component_set_id: String,
    pub(crate) node_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) json_blob_hash: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "derived_artifacts"]
pub(crate) struct DerivedArtifactRow {
    #[key]
    pub(crate) snapshot_id: String,
    #[key]
    pub(crate) kind: String,
    #[key]
    pub(crate) schema_version: i64,
    pub(crate) blob_hash: String,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "api_attempts"]
pub(crate) struct ApiAttemptRow {
    #[key]
    #[auto]
    pub(crate) id: i64,
    pub(crate) started_at: String,
    pub(crate) completed_at: String,
    pub(crate) command: String,
    pub(crate) endpoint_class: String,
    pub(crate) tier: String,
    pub(crate) file_key: Option<String>,
    pub(crate) request_profile: Option<String>,
    pub(crate) http_status: Option<i64>,
    pub(crate) error_code: Option<String>,
    pub(crate) retry_after: Option<i64>,
    pub(crate) plan_tier: Option<String>,
    pub(crate) rate_limit_type: Option<String>,
}

#[derive(Debug, Clone, toasty::Model)]
#[table = "schema_migrations"]
pub(crate) struct SchemaMigrationRow {
    #[key]
    pub(crate) version: i64,
    pub(crate) applied_at: String,
    pub(crate) checksum: String,
}
