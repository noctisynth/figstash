//! Figma REST, authentication, URL parsing, and quota-tier classification.
//!
//! All Figma HTTP access is isolated behind this crate's gateway.

mod auth;
mod gateway;
mod identifier;

pub use auth::{Credential, CredentialSource, KeyringBackend, PatProvider, SystemKeyring};
pub use gateway::{
    DownloadReceipt, FigmaGateway, FigmaTransport, RateLimitHeaders, ReqwestTransport,
    TransportError, TransportErrorKind, TransportResponse,
};
pub use identifier::{FigmaTarget, normalize_node_id, parse_figma_target};
