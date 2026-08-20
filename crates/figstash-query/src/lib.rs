//! Offline snapshot indexing, context transforms, search, and diff operations.
//!
//! This crate must remain independent of the Figma HTTP client so local
//! queries cannot trigger implicit network requests.
