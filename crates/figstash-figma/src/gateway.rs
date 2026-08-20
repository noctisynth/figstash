//! Quota-aware Figma HTTP gateway with a mockable streaming transport.

use crate::Credential;
use async_trait::async_trait;
use chrono::Utc;
use figstash_core::{
    ApiAttempt, AppError, AppResult, AttemptRecorder, CredentialKind, EndpointClass, ErrorCode,
    GeometryMode, NetworkUsage, RequestProfile, Tier,
};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::redirect::Policy;
use serde::Serialize;
use serde_json::json;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use url::Url;

const API_ROOT: &str = "https://api.figma.com/v1/";

/// Rate-limit response headers safe to persist and expose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitHeaders {
    /// Retry delay in seconds.
    pub retry_after: Option<u64>,
    /// Figma plan tier.
    pub plan_tier: Option<String>,
    /// Figma rate-limit type.
    pub rate_limit_type: Option<String>,
}

/// Successful transport response streamed to the requested staging path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    /// HTTP status code.
    pub status: u16,
    /// Safe rate-limit headers.
    pub rate_limit: RateLimitHeaders,
    /// Number of response bytes written.
    pub size: u64,
    /// BLAKE3 hash of uncompressed response bytes.
    pub hash: String,
}

/// Failure category produced below the Figma protocol mapping layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
    /// Connection establishment or transfer failed.
    Connection,
    /// Configured request timeout elapsed.
    Timeout,
    /// Local staging I/O failed.
    Io,
    /// Response exceeded the configured maximum size.
    TooLarge,
}

/// Redacted transport failure.
#[derive(Debug, Clone)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: String,
}

impl TransportError {
    /// Creates a redacted transport failure.
    pub fn new(kind: TransportErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the failure category.
    #[must_use]
    pub const fn kind(&self) -> TransportErrorKind {
        self.kind
    }
}

impl Display for TransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

/// Streaming transport boundary used to prohibit real network calls in tests.
#[async_trait]
pub trait FigmaTransport: Send + Sync {
    /// Downloads one classified Figma response to a staging file.
    async fn download(
        &self,
        url: &Url,
        endpoint: EndpointClass,
        credential: &Credential,
        destination: &Path,
        maximum_bytes: u64,
    ) -> Result<TransportResponse, TransportError>;
}

/// Production `reqwest` + rustls streaming transport.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Builds the production transport with safe redirects and configurable proxy inheritance.
    ///
    /// # Errors
    ///
    /// Returns an internal configuration error if the HTTP client cannot be built.
    pub fn new(
        connect_timeout: Duration,
        total_timeout: Duration,
        inherit_proxy: bool,
    ) -> AppResult<Self> {
        let redirect = Policy::custom(|attempt| {
            let url = attempt.url();
            if url.scheme() == "https" && url.host_str() == Some("api.figma.com") {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let mut builder = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .redirect(redirect);
        if !inherit_proxy {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                "Failed to construct the Figma HTTP client.",
            )
            .with_detail("reason", json!(error.to_string()))
        })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl FigmaTransport for ReqwestTransport {
    async fn download(
        &self,
        url: &Url,
        _endpoint: EndpointClass,
        credential: &Credential,
        destination: &Path,
        maximum_bytes: u64,
    ) -> Result<TransportResponse, TransportError> {
        let mut request = self.client.get(url.clone());
        request = match credential.kind() {
            CredentialKind::Pat | CredentialKind::Plan => {
                request.header("X-Figma-Token", credential.expose())
            }
            CredentialKind::OAuth => {
                let value = HeaderValue::from_str(&format!("Bearer {}", credential.expose()))
                    .map_err(|_| {
                        TransportError::new(
                            TransportErrorKind::Io,
                            "The credential cannot be represented as an HTTP header.",
                        )
                    })?;
                request.header(AUTHORIZATION, value)
            }
        };
        let response = request.send().await.map_err(reqwest_error)?;
        let status = response.status().as_u16();
        let rate_limit = parse_rate_limit_headers(response.headers());
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(destination)
            .await
            .map_err(|error| TransportError::new(TransportErrorKind::Io, error.to_string()))?;
        let mut size = 0_u64;
        let mut hasher = blake3::Hasher::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(reqwest_error)?;
            let chunk_size = u64::try_from(chunk.len()).map_err(|error| {
                TransportError::new(TransportErrorKind::TooLarge, error.to_string())
            })?;
            size = size.checked_add(chunk_size).ok_or_else(|| {
                TransportError::new(
                    TransportErrorKind::TooLarge,
                    "The Figma response size overflowed the local counter.",
                )
            })?;
            if size > maximum_bytes {
                return Err(TransportError::new(
                    TransportErrorKind::TooLarge,
                    "The Figma response exceeded the configured maximum download size.",
                ));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| TransportError::new(TransportErrorKind::Io, error.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|error| TransportError::new(TransportErrorKind::Io, error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| TransportError::new(TransportErrorKind::Io, error.to_string()))?;
        Ok(TransportResponse {
            status,
            rate_limit,
            size,
            hash: hasher.finalize().to_hex().to_string(),
        })
    }
}

/// Successful gateway download plus observed network usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadReceipt {
    /// Staging file containing the complete raw response.
    #[serde(skip_serializing)]
    pub path: PathBuf,
    /// Uncompressed BLAKE3 response hash.
    pub hash: String,
    /// Uncompressed response size.
    pub size: u64,
    /// Actual classified request attempts.
    pub network: NetworkUsage,
}

/// The only application boundary allowed to issue Figma REST requests.
pub struct FigmaGateway<T, L> {
    transport: T,
    ledger: L,
    maximum_bytes: u64,
}

impl<T, L> FigmaGateway<T, L>
where
    T: FigmaTransport,
    L: AttemptRecorder,
{
    /// Creates a classified gateway with a durable attempt recorder.
    pub const fn new(transport: T, ledger: L, maximum_bytes: u64) -> Self {
        Self {
            transport,
            ledger,
            maximum_bytes,
        }
    }

    /// Downloads one complete file snapshot. Tier 1 is never retried automatically.
    ///
    /// # Errors
    ///
    /// Returns classified auth, policy, network, Figma, ledger, or staging errors.
    pub async fn get_file(
        &self,
        file_key: &str,
        version: Option<&str>,
        profile: RequestProfile,
        credential: &Credential,
        destination: &Path,
    ) -> AppResult<DownloadReceipt> {
        let mut url = Url::parse(API_ROOT).map_err(|error| {
            AppError::new(
                ErrorCode::Internal,
                "The built-in Figma API URL is invalid.",
            )
            .with_detail("reason", json!(error.to_string()))
        })?;
        url.path_segments_mut()
            .map_err(|()| {
                AppError::new(
                    ErrorCode::Internal,
                    "The Figma API URL cannot accept a path.",
                )
            })?
            .extend(["files", file_key]);
        {
            let mut query = url.query_pairs_mut();
            if let Some(version) = version {
                query.append_pair("version", version);
            }
            if profile.geometry == GeometryMode::Paths {
                query.append_pair("geometry", "paths");
            }
        }
        self.execute_classified(
            "snapshot.pull",
            EndpointClass::GetFile,
            Some(file_key),
            Some(profile.key()),
            &url,
            credential,
            destination,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_classified(
        &self,
        command: &str,
        endpoint: EndpointClass,
        file_key: Option<&str>,
        request_profile: Option<&str>,
        url: &Url,
        credential: &Credential,
        destination: &Path,
    ) -> AppResult<DownloadReceipt> {
        let tier = endpoint.tier();
        let maximum_attempts = if tier == Tier::Tier1 { 1 } else { 2 };
        let mut network = NetworkUsage::default();
        for attempt_number in 0..maximum_attempts {
            network.record(tier);
            let started_at = Utc::now();
            let response = self
                .transport
                .download(url, endpoint, credential, destination, self.maximum_bytes)
                .await;
            let completed_at = Utc::now();
            match response {
                Ok(response) => {
                    let mapped_error = map_http_error(response.status, &response.rate_limit);
                    self.ledger
                        .record_attempt(&ApiAttempt {
                            started_at,
                            completed_at,
                            command: command.to_owned(),
                            endpoint_class: endpoint,
                            tier,
                            file_key: file_key.map(ToOwned::to_owned),
                            request_profile: request_profile.map(ToOwned::to_owned),
                            http_status: Some(response.status),
                            error_code: mapped_error
                                .as_ref()
                                .map(|error| error.code().as_str().to_owned()),
                            retry_after: response.rate_limit.retry_after,
                            plan_tier: response.rate_limit.plan_tier.clone(),
                            rate_limit_type: response.rate_limit.rate_limit_type.clone(),
                        })
                        .map_err(|error| with_network(error, network, endpoint))?;
                    if let Some(error) = mapped_error {
                        let retry = response.status >= 500
                            && tier != Tier::Tier1
                            && attempt_number + 1 < maximum_attempts;
                        if retry {
                            continue;
                        }
                        return Err(with_network(error, network, endpoint));
                    }
                    return Ok(DownloadReceipt {
                        path: destination.to_path_buf(),
                        hash: response.hash,
                        size: response.size,
                        network,
                    });
                }
                Err(error) => {
                    let app_error = map_transport_error(&error);
                    self.ledger
                        .record_attempt(&ApiAttempt {
                            started_at,
                            completed_at,
                            command: command.to_owned(),
                            endpoint_class: endpoint,
                            tier,
                            file_key: file_key.map(ToOwned::to_owned),
                            request_profile: request_profile.map(ToOwned::to_owned),
                            http_status: None,
                            error_code: Some(app_error.code().as_str().to_owned()),
                            retry_after: None,
                            plan_tier: None,
                            rate_limit_type: None,
                        })
                        .map_err(|error| with_network(error, network, endpoint))?;
                    let retry = matches!(
                        error.kind(),
                        TransportErrorKind::Connection | TransportErrorKind::Timeout
                    ) && tier != Tier::Tier1
                        && attempt_number + 1 < maximum_attempts;
                    if retry {
                        continue;
                    }
                    return Err(with_network(app_error, network, endpoint));
                }
            }
        }
        Err(with_network(
            AppError::new(
                ErrorCode::Internal,
                "The gateway retry loop produced no result.",
            ),
            network,
            endpoint,
        ))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn reqwest_error(error: reqwest::Error) -> TransportError {
    let kind = if error.is_timeout() {
        TransportErrorKind::Timeout
    } else {
        TransportErrorKind::Connection
    };
    TransportError::new(kind, error.to_string())
}

fn parse_rate_limit_headers(headers: &reqwest::header::HeaderMap) -> RateLimitHeaders {
    let string = |name: &'static str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned)
    };
    RateLimitHeaders {
        retry_after: string("retry-after").and_then(|value| value.parse().ok()),
        plan_tier: string("x-figma-plan-tier"),
        rate_limit_type: string("x-figma-rate-limit-type"),
    }
}

fn map_http_error(status: u16, headers: &RateLimitHeaders) -> Option<AppError> {
    let error = match status {
        200..=299 => return None,
        401 => AppError::new(ErrorCode::AuthFailed, "Figma rejected the credential."),
        403 => AppError::new(
            ErrorCode::ScopeMissing,
            "The credential cannot read this Figma file or lacks `file_content:read`.",
        ),
        404 => AppError::new(
            ErrorCode::FigmaError,
            "Figma did not find the requested file or version.",
        ),
        429 => AppError::new(ErrorCode::RateLimited, "Figma rate-limited the request.")
            .with_details(json!({
                "retryAfter": headers.retry_after,
                "planTier": headers.plan_tier,
                "rateLimitType": headers.rate_limit_type,
            }))
            .retryable(true),
        500..=599 => AppError::new(
            ErrorCode::FigmaError,
            "Figma returned a server error while fetching the file.",
        )
        .with_detail("httpStatus", json!(status))
        .retryable(true),
        _ => AppError::new(
            ErrorCode::FigmaError,
            "Figma returned an unexpected HTTP response.",
        )
        .with_detail("httpStatus", json!(status)),
    };
    Some(error)
}

fn map_transport_error(error: &TransportError) -> AppError {
    let code = if error.kind() == TransportErrorKind::TooLarge {
        ErrorCode::InvalidFigmaResponse
    } else if error.kind() == TransportErrorKind::Io {
        ErrorCode::StoreFailed
    } else {
        ErrorCode::NetworkFailed
    };
    AppError::new(code, "The Figma response could not be downloaded safely.")
        .with_detail("reason", json!(error.to_string()))
        .retryable(matches!(
            error.kind(),
            TransportErrorKind::Connection | TransportErrorKind::Timeout
        ))
}

fn with_network(error: AppError, network: NetworkUsage, endpoint: EndpointClass) -> AppError {
    error
        .with_detail("network", json!(network))
        .with_detail("endpoint", json!(endpoint))
        .with_detail("tier", json!(endpoint.tier()))
}

#[cfg(test)]
mod tests {
    use super::{
        FigmaGateway, FigmaTransport, RateLimitHeaders, TransportError, TransportErrorKind,
        TransportResponse,
    };
    use crate::{Credential, CredentialSource};
    use async_trait::async_trait;
    use figstash_core::{
        ApiAttempt, AppResult, AttemptRecorder, EndpointClass, ErrorCode, RequestProfile,
    };
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use url::Url;

    #[derive(Clone)]
    struct MemoryLedger(Arc<Mutex<Vec<ApiAttempt>>>);

    impl AttemptRecorder for MemoryLedger {
        fn record_attempt(&self, attempt: &ApiAttempt) -> AppResult<()> {
            self.0
                .lock()
                .unwrap_or_else(|error| panic!("ledger mutex poisoned: {error}"))
                .push(attempt.clone());
            Ok(())
        }
    }

    struct MockTransport {
        responses: Mutex<Vec<Result<TransportResponse, TransportError>>>,
    }

    #[async_trait]
    impl FigmaTransport for MockTransport {
        async fn download(
            &self,
            _url: &Url,
            _endpoint: EndpointClass,
            _credential: &Credential,
            _destination: &Path,
            _maximum_bytes: u64,
        ) -> Result<TransportResponse, TransportError> {
            self.responses
                .lock()
                .unwrap_or_else(|error| panic!("response mutex poisoned: {error}"))
                .remove(0)
        }
    }

    #[tokio::test]
    async fn tier_one_rate_limit_is_recorded_and_never_retried() {
        let ledger = MemoryLedger(Arc::new(Mutex::new(Vec::new())));
        let transport = MockTransport {
            responses: Mutex::new(vec![Ok(TransportResponse {
                status: 429,
                rate_limit: RateLimitHeaders {
                    retry_after: Some(60),
                    plan_tier: Some("starter".to_owned()),
                    rate_limit_type: Some("low".to_owned()),
                },
                size: 0,
                hash: String::new(),
            })]),
        };
        let gateway = FigmaGateway::new(transport, ledger.clone(), 1024);
        let credential = Credential::personal_access_token("secret", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        let error = gateway
            .get_file(
                "Abcdef123",
                None,
                RequestProfile::default(),
                &credential,
                Path::new("unused"),
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("429 must fail"));
        assert_eq!(error.code(), ErrorCode::RateLimited);
        assert_eq!(error.exit_code().as_i32(), 6);
        assert_eq!(
            ledger
                .0
                .lock()
                .unwrap_or_else(|poison| panic!("ledger mutex poisoned: {poison}"))
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn classifies_success_auth_not_found_server_and_timeout_without_tier_one_retries() {
        let credential = Credential::personal_access_token("secret", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        for (status, expected) in [
            (403, ErrorCode::ScopeMissing),
            (404, ErrorCode::FigmaError),
            (503, ErrorCode::FigmaError),
        ] {
            let ledger = MemoryLedger(Arc::new(Mutex::new(Vec::new())));
            let gateway = FigmaGateway::new(
                MockTransport {
                    responses: Mutex::new(vec![Ok(response(status))]),
                },
                ledger.clone(),
                1024,
            );
            let error = gateway
                .get_file(
                    "Abcdef123",
                    None,
                    RequestProfile::default(),
                    &credential,
                    Path::new("unused"),
                )
                .await
                .err()
                .unwrap_or_else(|| panic!("status {status} must fail"));
            assert_eq!(error.code(), expected);
            assert_eq!(ledger_len(&ledger), 1);
        }

        let ledger = MemoryLedger(Arc::new(Mutex::new(Vec::new())));
        let gateway = FigmaGateway::new(
            MockTransport {
                responses: Mutex::new(vec![Err(TransportError::new(
                    TransportErrorKind::Timeout,
                    "synthetic timeout",
                ))]),
            },
            ledger.clone(),
            1024,
        );
        let timeout = gateway
            .get_file(
                "Abcdef123",
                None,
                RequestProfile::default(),
                &credential,
                Path::new("unused"),
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("timeout must fail"));
        assert_eq!(timeout.code(), ErrorCode::NetworkFailed);
        assert_eq!(ledger_len(&ledger), 1);

        let ledger = MemoryLedger(Arc::new(Mutex::new(Vec::new())));
        let gateway = FigmaGateway::new(
            MockTransport {
                responses: Mutex::new(vec![Ok(response(200))]),
            },
            ledger.clone(),
            1024,
        );
        let receipt = gateway
            .get_file(
                "Abcdef123",
                Some("42"),
                RequestProfile::default(),
                &credential,
                Path::new("unused"),
            )
            .await
            .unwrap_or_else(|error| panic!("success failed: {error}"));
        assert_eq!(receipt.network.attempts, 1);
        assert_eq!(ledger_len(&ledger), 1);
    }

    #[tokio::test]
    async fn tier_two_and_three_policy_retries_one_safe_server_failure() {
        let credential = Credential::personal_access_token("secret", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        let ledger = MemoryLedger(Arc::new(Mutex::new(Vec::new())));
        let gateway = FigmaGateway::new(
            MockTransport {
                responses: Mutex::new(vec![Ok(response(503)), Ok(response(200))]),
            },
            ledger.clone(),
            1024,
        );
        let url = Url::parse("https://api.figma.com/v1/files/Abcdef123/images")
            .unwrap_or_else(|error| panic!("test URL failed: {error}"));
        let receipt = gateway
            .execute_classified(
                "test.image_fills",
                EndpointClass::GetImageFills,
                Some("Abcdef123"),
                None,
                &url,
                &credential,
                Path::new("unused"),
            )
            .await
            .unwrap_or_else(|error| panic!("safe retry failed: {error}"));
        assert_eq!(receipt.network.attempts, 2);
        assert_eq!(receipt.network.tier2, 2);
        assert_eq!(ledger_len(&ledger), 2);
    }

    fn response(status: u16) -> TransportResponse {
        TransportResponse {
            status,
            rate_limit: RateLimitHeaders::default(),
            size: 2,
            hash: blake3::hash(b"{}").to_hex().to_string(),
        }
    }

    fn ledger_len(ledger: &MemoryLedger) -> usize {
        ledger
            .0
            .lock()
            .unwrap_or_else(|error| panic!("ledger mutex poisoned: {error}"))
            .len()
    }
}
