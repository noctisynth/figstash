//! Storage error conversion helpers.

use figstash_core::{AppError, ErrorCode};
use serde_json::json;

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn io_error(context: &'static str, error: std::io::Error) -> AppError {
    AppError::new(ErrorCode::StoreFailed, context)
        .with_details(json!({"reason": error.to_string()}))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sql_error(context: &'static str, error: impl ToString) -> AppError {
    AppError::new(ErrorCode::StoreFailed, context)
        .with_details(json!({"reason": error.to_string()}))
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn corrupt_error(context: &'static str, reason: impl ToString) -> AppError {
    AppError::new(ErrorCode::StoreCorrupt, context)
        .with_details(json!({"reason": reason.to_string()}))
}
