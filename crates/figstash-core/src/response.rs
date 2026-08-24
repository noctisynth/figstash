//! Stable machine-readable CLI envelopes.

use crate::{AppError, CLI_SCHEMA_VERSION, Tier};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Origin of the data returned by a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseSource {
    /// Data came exclusively from the local snapshot store.
    Cache,
    /// Data came exclusively from Figma.
    Figma,
    /// Data combines local and Figma sources.
    Mixed,
    /// The command did not return source-backed data.
    None,
}

/// Observed network attempts made by the command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkUsage {
    /// Total attempted Figma HTTP requests.
    pub attempts: u32,
    /// Attempted Tier 1 requests.
    pub tier1: u32,
    /// Attempted Tier 2 requests.
    pub tier2: u32,
    /// Attempted Tier 3 requests.
    pub tier3: u32,
}

impl NetworkUsage {
    /// Records an attempted classified endpoint.
    pub const fn record(&mut self, tier: Tier) {
        self.attempts += 1;
        match tier {
            Tier::Tier1 => self.tier1 += 1,
            Tier::Tier2 => self.tier2 += 1,
            Tier::Tier3 => self.tier3 += 1,
        }
    }
}

/// A non-fatal condition attached to a successful response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    /// Stable warning code.
    pub code: String,
    /// Human-readable description.
    pub message: String,
    /// Structured warning context.
    pub details: Value,
}

impl Warning {
    /// Creates a warning without structured context.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Value::Object(serde_json::Map::new()),
        }
    }

    /// Adds structured warning context.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
}

/// Command metadata common to successful and failed envelopes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// Canonical command name.
    pub command: String,
    /// Origin of returned data.
    pub source: ResponseSource,
    /// Resolved immutable snapshot identifier, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Figma file version captured by the snapshot, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub figma_version: Option<String>,
    /// Observed network usage for this invocation.
    pub network: NetworkUsage,
    /// Wall-clock command duration in milliseconds, when collected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Per-invocation trace identifier.
    pub trace_id: String,
}

impl Meta {
    /// Creates metadata for a command and source.
    pub fn new(
        command: impl Into<String>,
        source: ResponseSource,
        trace_id: impl Into<String>,
    ) -> Self {
        Self {
            command: command.into(),
            source,
            snapshot_id: None,
            figma_version: None,
            network: NetworkUsage::default(),
            duration_ms: None,
            trace_id: trace_id.into(),
        }
    }
}

/// Public error payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    /// Stable error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Structured context.
    pub details: Value,
    /// Whether retrying may succeed.
    pub retryable: bool,
}

impl From<&AppError> for ErrorBody {
    fn from(error: &AppError) -> Self {
        Self {
            code: error.code().as_str().to_owned(),
            message: error.message().to_owned(),
            details: error.details().clone(),
            retryable: error.is_retryable(),
        }
    }
}

/// Successful CLI response.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessEnvelope<T> {
    /// Envelope schema version.
    pub schema_version: u32,
    /// Always true for this variant.
    pub ok: bool,
    /// Command-specific response data.
    pub data: T,
    /// Command metadata.
    pub meta: Meta,
    /// Non-fatal warnings.
    pub warnings: Vec<Warning>,
}

/// Failed CLI response.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureEnvelope {
    /// Envelope schema version.
    pub schema_version: u32,
    /// Always false for this variant.
    pub ok: bool,
    /// Structured error payload.
    pub error: ErrorBody,
    /// Command metadata.
    pub meta: Meta,
    /// Warnings collected before the failure.
    pub warnings: Vec<Warning>,
}

/// Either successful or failed stable CLI output.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResponseEnvelope<T> {
    /// Successful response.
    Success(SuccessEnvelope<T>),
    /// Failed response.
    Failure(FailureEnvelope),
}

impl<T> ResponseEnvelope<T> {
    /// Creates a successful response.
    pub const fn success(data: T, meta: Meta, warnings: Vec<Warning>) -> Self {
        Self::Success(SuccessEnvelope {
            schema_version: CLI_SCHEMA_VERSION,
            ok: true,
            data,
            meta,
            warnings,
        })
    }

    /// Creates a failed response.
    #[must_use]
    pub fn failure(error: &AppError, meta: Meta, warnings: Vec<Warning>) -> Self {
        Self::Failure(FailureEnvelope {
            schema_version: CLI_SCHEMA_VERSION,
            ok: false,
            error: ErrorBody::from(error),
            meta,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{AppError, ErrorCode};

    use super::{Meta, ResponseEnvelope, ResponseSource};

    #[test]
    fn success_and_failure_have_mutually_exclusive_payloads() {
        let success = serde_json::to_value(ResponseEnvelope::success(
            serde_json::json!({"value": 1}),
            Meta::new("test", ResponseSource::Cache, "trace"),
            Vec::new(),
        ))
        .unwrap_or_else(|error| panic!("success envelope must serialize: {error}"));
        assert!(success.get("data").is_some());
        assert!(success.get("error").is_none());

        let failure = serde_json::to_value(ResponseEnvelope::<Value>::failure(
            &AppError::new(ErrorCode::InvalidInput, "invalid"),
            Meta::new("test", ResponseSource::None, "trace"),
            Vec::new(),
        ))
        .unwrap_or_else(|error| panic!("failure envelope must serialize: {error}"));
        assert!(failure.get("data").is_none());
        assert!(failure.get("error").is_some());
    }

    use serde_json::Value;
}
