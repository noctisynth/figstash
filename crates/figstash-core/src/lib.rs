//! Domain types and application contracts for Figstash.
//!
//! This crate is infrastructure-independent and must not depend on the CLI,
//! Figma transport, storage, or query implementations.

mod error;
mod model;
mod repository;
mod response;

pub use error::{AppError, AppResult, ErrorCode, ExitCode};
pub use model::{
    ApiAttempt, BoundingBox, ComponentUsage, CredentialKind, EndpointClass, GeometryMode,
    IndexedEntity, IndexedNode, IndexedSnapshot, NodeSearchQuery, NodeSearchResult, RequestProfile,
    SnapshotSelector, SnapshotSummary, StoredNode, Tier,
};
pub use repository::{AttemptRecorder, SnapshotRepository};
pub use response::{
    ErrorBody, FailureEnvelope, Meta, NetworkUsage, ResponseEnvelope, ResponseSource,
    SuccessEnvelope, Warning,
};

/// Version of the stable machine-readable CLI envelope.
pub const CLI_SCHEMA_VERSION: u32 = 1;

/// Version of the tolerant Figma document parser and derived indexes.
pub const PARSER_VERSION: u32 = 1;
