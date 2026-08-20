//! Offline snapshot indexing, context transforms, and search operations.
//!
//! This crate must remain independent of the Figma HTTP client so local
//! queries cannot trigger implicit network requests.

mod parser;
mod service;
mod transform;

pub use parser::parse_file;
pub use service::{
    ComponentsData, NodeGetData, NodeGetOptions, NodeReferences, QueryService, SearchData,
    TokensData, View,
};
pub use transform::{CompactNode, DerivedVariable, TokenReference};
