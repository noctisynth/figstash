//! Application errors and stable process exit codes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};

/// Result type shared across Figstash crates.
pub type AppResult<T> = Result<T, AppError>;

/// Stable machine-readable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// A value supplied by the caller is invalid.
    InvalidInput,
    /// The CLI arguments cannot be interpreted.
    InvalidArguments,
    /// No usable credential was found.
    AuthMissing,
    /// Figma rejected the credential.
    AuthFailed,
    /// The credential is missing a required scope.
    ScopeMissing,
    /// No local snapshot exists for the requested file/profile.
    SnapshotMissing,
    /// The explicitly selected snapshot does not exist.
    SnapshotNotFound,
    /// The requested node does not exist in the selected snapshot.
    NodeNotFound,
    /// The requested component does not exist in the selected snapshot.
    ComponentNotFound,
    /// A network operation failed before a valid Figma response was received.
    NetworkFailed,
    /// Figma returned a service error.
    FigmaError,
    /// Figma returned a response that cannot be stored safely.
    InvalidFigmaResponse,
    /// Figma rate-limited the request.
    RateLimited,
    /// Policy rejected the classified endpoint.
    EndpointNotAllowed,
    /// A network command was attempted in offline mode.
    OfflineMode,
    /// Refresh metadata is unavailable without spending a Tier 1 request.
    MetadataUnavailable,
    /// Another process currently owns the file/profile writer lock.
    StoreBusy,
    /// Local durable state is inconsistent or corrupt.
    StoreCorrupt,
    /// A local storage operation failed.
    StoreFailed,
    /// The requested command belongs to a later milestone.
    NotImplemented,
    /// An unexpected internal error occurred.
    Internal,
}

impl ErrorCode {
    /// Returns the stable JSON representation of the error code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::InvalidArguments => "invalid_arguments",
            Self::AuthMissing => "auth_missing",
            Self::AuthFailed => "auth_failed",
            Self::ScopeMissing => "scope_missing",
            Self::SnapshotMissing => "snapshot_missing",
            Self::SnapshotNotFound => "snapshot_not_found",
            Self::NodeNotFound => "node_not_found",
            Self::ComponentNotFound => "component_not_found",
            Self::NetworkFailed => "network_failed",
            Self::FigmaError => "figma_error",
            Self::InvalidFigmaResponse => "invalid_figma_response",
            Self::RateLimited => "rate_limited",
            Self::EndpointNotAllowed => "endpoint_not_allowed",
            Self::OfflineMode => "offline_mode",
            Self::MetadataUnavailable => "metadata_unavailable",
            Self::StoreBusy => "store_busy",
            Self::StoreCorrupt => "store_corrupt",
            Self::StoreFailed => "store_failed",
            Self::NotImplemented => "not_implemented",
            Self::Internal => "internal_error",
        }
    }

    /// Maps the error category to the stable process exit code.
    #[must_use]
    pub const fn exit_code(self) -> ExitCode {
        match self {
            Self::InvalidInput | Self::InvalidArguments => ExitCode::Input,
            Self::AuthMissing | Self::AuthFailed | Self::ScopeMissing => ExitCode::Auth,
            Self::SnapshotMissing
            | Self::SnapshotNotFound
            | Self::NodeNotFound
            | Self::ComponentNotFound => ExitCode::LocalData,
            Self::NetworkFailed | Self::FigmaError | Self::InvalidFigmaResponse => {
                ExitCode::Network
            }
            Self::RateLimited
            | Self::EndpointNotAllowed
            | Self::OfflineMode
            | Self::MetadataUnavailable => ExitCode::Policy,
            Self::StoreBusy | Self::StoreCorrupt | Self::StoreFailed => ExitCode::Storage,
            Self::NotImplemented | Self::Internal => ExitCode::Internal,
        }
    }
}

/// Stable process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Command completed successfully.
    Success = 0,
    /// Invalid input or arguments.
    Input = 2,
    /// Authentication or authorization failure.
    Auth = 3,
    /// Required local data is absent.
    LocalData = 4,
    /// Network or remote service failure.
    Network = 5,
    /// Policy prevented the requested operation.
    Policy = 6,
    /// Durable storage failure.
    Storage = 7,
    /// Internal or unimplemented behavior.
    Internal = 8,
}

impl ExitCode {
    /// Returns the integer process status.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

/// A structured application failure safe to expose through the CLI envelope.
#[derive(Debug, Clone)]
pub struct AppError {
    code: ErrorCode,
    message: String,
    details: Value,
    retryable: bool,
}

impl AppError {
    /// Creates an error without additional details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Value::Object(serde_json::Map::new()),
            retryable: false,
        }
    }

    /// Adds structured details safe for the public JSON response.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// Inserts one field into structured details, preserving existing fields.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: Value) -> Self {
        if !self.details.is_object() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Some(details) = self.details.as_object_mut() {
            details.insert(key.into(), value);
        }
        self
    }

    /// Marks whether retrying the same operation may succeed.
    #[must_use]
    pub const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Returns the stable error code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns structured details.
    #[must_use]
    pub const fn details(&self) -> &Value {
        &self.details
    }

    /// Returns whether retrying may succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Returns the stable process exit code.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        self.code.exit_code()
    }
}

impl Display for AppError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AppError {}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ExitCode};

    #[test]
    fn maps_error_categories_to_stable_exit_codes() {
        assert_eq!(ErrorCode::InvalidInput.exit_code(), ExitCode::Input);
        assert_eq!(ErrorCode::AuthMissing.exit_code(), ExitCode::Auth);
        assert_eq!(ErrorCode::NodeNotFound.exit_code(), ExitCode::LocalData);
        assert_eq!(ErrorCode::NetworkFailed.exit_code(), ExitCode::Network);
        assert_eq!(ErrorCode::RateLimited.exit_code(), ExitCode::Policy);
        assert_eq!(ErrorCode::OfflineMode.exit_code(), ExitCode::Policy);
        assert_eq!(ErrorCode::StoreBusy.exit_code(), ExitCode::Storage);
        assert_eq!(ErrorCode::Internal.exit_code(), ExitCode::Internal);
    }
}
