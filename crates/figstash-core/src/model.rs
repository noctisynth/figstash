//! Domain models shared by network, storage, query, and CLI layers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Credential kinds understood by the domain contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// Personal access token.
    Pat,
    /// Placeholder for a future plan-derived credential.
    Plan,
    /// Placeholder for a future OAuth credential.
    OAuth,
}

/// Exhaustive Figma endpoint classes permitted by the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointClass {
    /// `GET /v1/files/:key`.
    GetFile,
    /// `GET /v1/files/:key/nodes`.
    GetFileNodes,
    /// `GET /v1/images/:key`.
    GetImages,
    /// `GET /v1/files/:key/images`.
    GetImageFills,
    /// `GET /v1/me`.
    GetCurrentUser,
}

impl EndpointClass {
    /// Returns the quota tier assigned by the authoritative classifier.
    #[must_use]
    pub const fn tier(self) -> Tier {
        match self {
            Self::GetFile | Self::GetFileNodes | Self::GetImages => Tier::Tier1,
            Self::GetImageFills => Tier::Tier2,
            Self::GetCurrentUser => Tier::Tier3,
        }
    }
}

/// Figma endpoint quota tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Full document or node content.
    Tier1,
    /// Render or image-oriented endpoints.
    Tier2,
    /// Lightweight metadata endpoints.
    Tier3,
}

/// Geometry payload requested during a snapshot pull.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeometryMode {
    /// Do not request vector path geometry.
    #[default]
    None,
    /// Request vector path geometry.
    Paths,
}

/// Content-affecting options that define an independent snapshot lineage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestProfile {
    /// Geometry payload mode.
    pub geometry: GeometryMode,
}

impl RequestProfile {
    /// Returns the stable persistence key for this profile.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self.geometry {
            GeometryMode::None => "geometry=none",
            GeometryMode::Paths => "geometry=paths",
        }
    }
}

/// Absolute node bounds in document coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundingBox {
    /// Horizontal position.
    pub x: f64,
    /// Vertical position.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// A normalized node row plus its raw lossless representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedNode {
    /// Stable Figma node identifier.
    pub node_id: String,
    /// Parent node identifier, absent for the document root.
    pub parent_id: Option<String>,
    /// Figma node type string, including unknown future values.
    pub node_type: String,
    /// Human-readable node name.
    pub name: String,
    /// Depth below the document root.
    pub depth: u32,
    /// Zero-based position among siblings.
    pub sibling_order: u32,
    /// Root-to-node identifier path.
    pub path_ids: Vec<String>,
    /// Effective visibility flag when present.
    pub visible: Option<bool>,
    /// Absolute bounds when present.
    pub bounds: Option<BoundingBox>,
    /// Referenced main component identifier for instances.
    pub component_id: Option<String>,
    /// Text characters for text nodes.
    pub text_content: Option<String>,
    /// Complete raw node JSON.
    pub raw: Value,
    /// Deterministic hash of the normalized subtree.
    pub subtree_hash: String,
}

/// A style, component, or component-set entry indexed from the response maps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedEntity {
    /// Map key or Figma entity identifier.
    pub id: String,
    /// Owning node identifier when available.
    pub node_id: Option<String>,
    /// Human-readable name when available.
    pub name: Option<String>,
    /// Entity kind.
    pub entity_type: String,
    /// Complete raw entity JSON.
    pub raw: Value,
}

/// Parsed, normalized snapshot ready for a single durable transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSnapshot {
    /// Figma file name.
    pub file_name: String,
    /// Figma version returned by the API.
    pub figma_version: String,
    /// Last modified timestamp returned by Figma, if present.
    pub last_modified: Option<String>,
    /// Flattened node index in deterministic preorder.
    pub nodes: Vec<IndexedNode>,
    /// Indexed style map.
    pub styles: Vec<IndexedEntity>,
    /// Indexed component map.
    pub components: Vec<IndexedEntity>,
    /// Indexed component-set map.
    pub component_sets: Vec<IndexedEntity>,
    /// Parser warnings retained with the snapshot.
    pub warnings: Vec<crate::Warning>,
}

/// Durable metadata for one immutable snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSummary {
    /// Locally generated immutable snapshot identifier.
    pub id: String,
    /// Canonical Figma file key.
    pub file_key: String,
    /// Captured Figma file version.
    pub figma_version: String,
    /// Stable request profile key.
    pub request_profile: String,
    /// Captured Figma file name.
    pub file_name: String,
    /// Local fetch completion time.
    pub fetched_at: DateTime<Utc>,
    /// Figma last-modified timestamp, when present.
    pub last_modified: Option<String>,
    /// Content-addressed raw blob hash.
    pub raw_blob_hash: String,
    /// Uncompressed response size in bytes.
    pub raw_size: u64,
    /// Indexed node count.
    pub node_count: u64,
    /// Parser/index format version.
    pub parser_version: u32,
    /// Whether this snapshot is the current lineage head.
    pub is_head: bool,
}

/// Resolves either a named immutable snapshot or the current lineage head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSelector<'a> {
    /// Canonical Figma file key.
    pub file_key: &'a str,
    /// Stable request profile key.
    pub request_profile: &'a str,
    /// Explicit snapshot identifier, or `None` for `HEAD`.
    pub snapshot_id: Option<&'a str>,
}

/// Node row returned by the repository with ordered direct children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredNode {
    /// Normalized node row.
    pub index: IndexedNode,
    /// Direct child identifiers in document order.
    pub child_ids: Vec<String>,
}

/// Lightweight indexed node state used for bulk local snapshot comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDiffNode {
    /// Stable Figma node identifier.
    pub node_id: String,
    /// Direct parent node identifier.
    pub parent_id: Option<String>,
    /// Figma node type string.
    pub node_type: String,
    /// Human-readable node name.
    pub name: String,
    /// Zero-based position among siblings.
    pub sibling_order: u32,
    /// Root-to-node identifier path.
    pub path_ids: Vec<String>,
    /// Content-addressed hash of this node's own raw fields, excluding children.
    pub own_hash: String,
    /// Deterministic hash of this node and its ordered descendants.
    pub subtree_hash: String,
}

/// Pure-local node search filters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSearchQuery {
    /// Case-insensitive name substring.
    pub name: Option<String>,
    /// Full-text content query.
    pub text: Option<String>,
    /// Exact Figma node type.
    pub node_type: Option<String>,
    /// Restrict results to descendants of this node.
    pub ancestor_id: Option<String>,
    /// Maximum number of returned rows.
    pub limit: u32,
    /// Opaque deterministic pagination cursor.
    pub cursor: Option<String>,
}

/// Compact node search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSearchResult {
    /// Node identifier.
    pub node_id: String,
    /// Node name.
    pub name: String,
    /// Figma node type.
    pub node_type: String,
    /// Root-to-node identifier path.
    pub path_ids: Vec<String>,
    /// Depth below the document root.
    pub depth: u32,
}

/// Instances referencing a component in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentUsage {
    /// Referenced component identifier.
    pub component_id: String,
    /// Matching instance node identifiers.
    pub instance_node_ids: Vec<String>,
}

/// One locally observed Figma API attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiAttempt {
    /// UTC attempt start time.
    pub started_at: DateTime<Utc>,
    /// UTC attempt completion time.
    pub completed_at: DateTime<Utc>,
    /// Canonical CLI command.
    pub command: String,
    /// Exhaustively classified endpoint.
    pub endpoint_class: EndpointClass,
    /// Assigned quota tier.
    pub tier: Tier,
    /// File key when applicable.
    pub file_key: Option<String>,
    /// Request profile when applicable.
    pub request_profile: Option<String>,
    /// HTTP status, if a response was received.
    pub http_status: Option<u16>,
    /// Stable error code, if the attempt failed.
    pub error_code: Option<String>,
    /// Parsed retry-after seconds, if provided.
    pub retry_after: Option<u64>,
    /// Observed Figma plan tier header, if provided.
    pub plan_tier: Option<String>,
    /// Observed Figma rate-limit type header, if provided.
    pub rate_limit_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{EndpointClass, GeometryMode, RequestProfile, Tier};

    #[test]
    fn endpoint_classifier_is_exhaustive_and_stable() {
        assert_eq!(EndpointClass::GetFile.tier(), Tier::Tier1);
        assert_eq!(EndpointClass::GetFileNodes.tier(), Tier::Tier1);
        assert_eq!(EndpointClass::GetImages.tier(), Tier::Tier1);
        assert_eq!(EndpointClass::GetImageFills.tier(), Tier::Tier2);
        assert_eq!(EndpointClass::GetCurrentUser.tier(), Tier::Tier3);
    }

    #[test]
    fn request_profile_key_is_content_sensitive() {
        assert_eq!(RequestProfile::default().key(), "geometry=none");
        assert_eq!(
            RequestProfile {
                geometry: GeometryMode::Paths,
            }
            .key(),
            "geometry=paths"
        );
    }
}
