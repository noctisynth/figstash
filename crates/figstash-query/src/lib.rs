//! Offline snapshot indexing, context transforms, and search operations.
//!
//! This crate must remain independent of the Figma HTTP client so local
//! queries cannot trigger implicit network requests.

mod diff;
mod parser;
mod service;
mod transform;

pub use diff::{
    DiffNodeChange, DiffNodeState, EntityChange, EntityChangesByKind, EntityDiff, EntityDiffCounts,
    NodeChanges, SnapshotDiffData, SnapshotDiffFilters, SnapshotDiffSummary,
};
pub use parser::parse_file;
pub use service::{
    ComponentsData, NodeGetData, NodeGetOptions, NodeReferences, OutlineData, OutlineNode,
    QueryService, SearchData, TokensData, View,
};
pub use transform::{CompactNode, DerivedVariable, TokenReference};
