//! CLI application composition.

use crate::args::{
    AuthSubcommand, Cli, Command, ComponentsSubcommand, ContextArgs, GeometryArg, NodeSubcommand,
    OutlineArgs, QuotaSubcommand, SchemaArgs, SnapshotSubcommand, TokensSubcommand, ViewArg,
};
use crate::config::AppConfig;
use crate::output::{CommandOutput, emit_failure, emit_success};
use figstash_core::{
    AppError, AppResult, AttemptRecorder, ErrorCode, GeometryMode, NetworkUsage, RequestProfile,
    ResponseSource, SnapshotRepository, SnapshotSelector, SnapshotSummary,
};
use figstash_figma::{
    Credential, FigmaGateway, FigmaTarget, FigmaTransport, PatProvider, ReqwestTransport,
    SystemKeyring, normalize_node_id, parse_figma_target,
};
use figstash_query::{NodeGetOptions, QueryService, View, parse_file};
use figstash_store::Store;
use serde_json::json;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Instant;
use tokio::net::TcpStream;
use tracing_subscriber::EnvFilter;

pub(crate) async fn run(cli: Cli) -> i32 {
    let command = cli.command_name();
    let started = Instant::now();
    let config = match AppConfig::load(&cli) {
        Ok(config) => config,
        Err(error) => {
            emit_failure(command, &error, Some(started));
            return error.exit_code().as_i32();
        }
    };
    initialize_logging(&config.log_level);
    match dispatch(&cli, &config).await {
        Ok(output) => {
            emit_success(command, output, started);
            0
        }
        Err(error) => {
            emit_failure(command, &error, Some(started));
            error.exit_code().as_i32()
        }
    }
}

async fn dispatch(cli: &Cli, config: &AppConfig) -> AppResult<CommandOutput> {
    match &cli.command {
        Command::Context(args) => context_get(config, args),
        Command::Outline(args) => outline_get(config, args),
        Command::Schema(args) => schema_get(args),
        Command::Auth(auth) => auth_command(config, cli.offline, &auth.command).await,
        Command::Doctor(args) => doctor(config, cli.offline, args.network).await,
        Command::Quota(quota) => match quota.command {
            QuotaSubcommand::Status => {
                serialize_local(Store::open(&config.data_dir)?.quota_status()?)
            }
        },
        Command::Snapshot(snapshot) => match &snapshot.command {
            SnapshotSubcommand::Pull(args) => snapshot_pull(config, cli.offline, args).await,
            SnapshotSubcommand::Status(args) => {
                let target = parse_figma_target(&args.target)?;
                let profile = request_profile(args.geometry);
                let summary =
                    Store::open(&config.data_dir)?.resolve_snapshot(SnapshotSelector {
                        file_key: target.effective_file_key(),
                        request_profile: profile.key(),
                        snapshot_id: args.snapshot.as_deref(),
                    })?;
                local_with_snapshot(
                    json!({
                        "snapshot": summary,
                        "stale": config.stale_after.is_some_and(|duration| {
                            let age = chrono_like_age(summary.fetched_at.timestamp());
                            age.is_some_and(|age| age > duration)
                        })
                    }),
                    &summary,
                )
            }
            SnapshotSubcommand::List(args) => {
                let file_key = args.file.as_deref().map(parse_figma_target).transpose()?;
                serialize_local(json!({
                    "snapshots": Store::open(&config.data_dir)?
                        .list_snapshots(file_key.as_ref().map(figstash_figma::FigmaTarget::effective_file_key))?
                }))
            }
            SnapshotSubcommand::Diff(args) => {
                let target = parse_figma_target(&args.target)?;
                Err(AppError::new(
                    ErrorCode::NotImplemented,
                    "Snapshot diff is scheduled for P1 and is not implemented in the P0 CLI.",
                )
                .with_details(json!({
                    "fileKey": target.effective_file_key(),
                    "snapshotA": args.snapshot_a,
                    "snapshotB": args.snapshot_b,
                })))
            }
            SnapshotSubcommand::Prune(args) => {
                let store = Store::open(&config.data_dir)?;
                if args.execute {
                    serialize_local(store.execute_prune()?)
                } else {
                    serialize_local(store.prune_plan()?)
                }
            }
        },
        Command::Node(node) => match &node.command {
            NodeSubcommand::Get(args) => node_get(config, args),
            NodeSubcommand::Search(args) => node_search(config, args),
        },
        Command::Tokens(tokens) => match &tokens.command {
            TokensSubcommand::Get(args) => {
                let target = parse_figma_target(&args.target)?;
                let profile = request_profile(args.geometry);
                let data = QueryService::new(Store::open(&config.data_dir)?).tokens_get(
                    SnapshotSelector {
                        file_key: target.effective_file_key(),
                        request_profile: profile.key(),
                        snapshot_id: args.snapshot.as_deref(),
                    },
                )?;
                local_with_snapshot(&data, &data.snapshot)
            }
        },
        Command::Components(components) => match &components.command {
            ComponentsSubcommand::List(args) => {
                let target = parse_figma_target(&args.target)?;
                let profile = request_profile(args.geometry);
                let data = QueryService::new(Store::open(&config.data_dir)?).components_list(
                    SnapshotSelector {
                        file_key: target.effective_file_key(),
                        request_profile: profile.key(),
                        snapshot_id: args.snapshot.as_deref(),
                    },
                )?;
                local_with_snapshot(&data, &data.snapshot)
            }
        },
    }
}

fn context_get(config: &AppConfig, args: &ContextArgs) -> AppResult<CommandOutput> {
    let (target, node_id) = target_and_node(&args.target, args.node.as_deref())?;
    let node_id = node_id.ok_or_else(|| {
        AppError::new(
            ErrorCode::NodeRequired,
            "Design context requires a node URL or an explicit `--node`.",
        )
        .with_details(json!({
            "fileKey": target.effective_file_key(),
            "suggestedCommand": format!("figstash outline {}", target.effective_file_key()),
        }))
    })?;
    let profile = request_profile(args.geometry);
    let data = QueryService::new(Store::open(&config.data_dir)?).node_get(NodeGetOptions {
        selector: SnapshotSelector {
            file_key: target.effective_file_key(),
            request_profile: profile.key(),
            snapshot_id: args.snapshot.as_deref(),
        },
        node_id: Some(&node_id),
        depth: args.depth,
        view: View::Compact,
    })?;
    local_with_snapshot(&data, &data.snapshot)
}

fn outline_get(config: &AppConfig, args: &OutlineArgs) -> AppResult<CommandOutput> {
    let (target, node_id) = target_and_node(&args.target, args.node.as_deref())?;
    let profile = request_profile(args.geometry);
    let data = QueryService::new(Store::open(&config.data_dir)?).outline(
        SnapshotSelector {
            file_key: target.effective_file_key(),
            request_profile: profile.key(),
            snapshot_id: args.snapshot.as_deref(),
        },
        node_id.as_deref(),
        args.depth,
    )?;
    local_with_snapshot(&data, &data.snapshot)
}

fn schema_get(args: &SchemaArgs) -> AppResult<CommandOutput> {
    match args.command.as_deref() {
        Some(command) => serialize_neutral(crate::schema_catalog::contract(command)?),
        None => serialize_neutral(crate::schema_catalog::catalog()),
    }
}

async fn auth_command(
    config: &AppConfig,
    offline: bool,
    command: &AuthSubcommand,
) -> AppResult<CommandOutput> {
    let provider = PatProvider::new(SystemKeyring);
    match command {
        AuthSubcommand::Set(args) => {
            if !args.stdin {
                return Err(AppError::new(
                    ErrorCode::InvalidArguments,
                    "Personal access tokens may only be supplied through stdin.",
                ));
            }
            let mut secret = String::new();
            std::io::stdin()
                .take(64 * 1024 + 1)
                .read_to_string(&mut secret)
                .map_err(|error| {
                    AppError::new(ErrorCode::AuthFailed, "Failed to read the PAT from stdin.")
                        .with_detail("reason", json!(error.to_string()))
                })?;
            if secret.len() > 64 * 1024 {
                return Err(AppError::new(
                    ErrorCode::InvalidInput,
                    "The personal access token exceeds the supported input size.",
                ));
            }
            while secret.ends_with('\r') || secret.ends_with('\n') {
                secret.pop();
            }
            provider.store(secret)?;
            serialize_neutral(json!({
                "credentialKind": "pat",
                "source": "keyring",
                "present": true,
                "remoteValidated": false
            }))
        }
        AuthSubcommand::Status => {
            let source = provider.status()?;
            serialize_neutral(json!({
                "credentialKind": source.map(|_| "pat"),
                "source": source,
                "present": source.is_some(),
                "remoteValidated": false
            }))
        }
        AuthSubcommand::Whoami => {
            if offline {
                return Err(AppError::new(
                    ErrorCode::OfflineMode,
                    "Current-user lookup is disabled by `--offline`.",
                ));
            }
            let credential = provider.resolve()?;
            let store = Store::open(&config.data_dir)?;
            let stage = store.create_staging_file()?;
            let transport = ReqwestTransport::new(
                config.connect_timeout,
                config.total_timeout,
                config.inherit_proxy,
            )?;
            let gateway = FigmaGateway::new(transport, store, config.maximum_download_bytes);
            let result = whoami_with_gateway(&gateway, &credential, &stage).await;
            cleanup_stage(&stage);
            result
        }
        AuthSubcommand::Clear => {
            let cleared = provider.clear()?;
            serialize_neutral(json!({
                "cleared": cleared,
                "keyringPresent": false,
                "environmentPresent": std::env::var_os("FIGMA_TOKEN").is_some()
            }))
        }
    }
}

async fn whoami_with_gateway<T, L>(
    gateway: &FigmaGateway<T, L>,
    credential: &Credential,
    destination: &Path,
) -> AppResult<CommandOutput>
where
    T: FigmaTransport,
    L: AttemptRecorder,
{
    let receipt = gateway.get_current_user(credential, destination).await?;
    serialize_figma(
        json!({
            "credentialKind": credential.kind(),
            "credentialSource": credential.source(),
            "remoteValidated": true,
            "user": receipt.user,
        }),
        receipt.network,
    )
}

async fn doctor(config: &AppConfig, offline: bool, network: bool) -> AppResult<CommandOutput> {
    let store = Store::open(&config.data_dir)?;
    let credential_source = PatProvider::new(SystemKeyring).status().ok().flatten();
    let mut checks = vec![
        json!({"name": "data_directory", "ok": store.root().is_dir(), "path": store.root()}),
        json!({"name": "config_directory", "ok": true, "path": config.config_dir}),
        json!({"name": "credential_present", "ok": credential_source.is_some(), "source": credential_source}),
    ];
    if network {
        if offline {
            return Err(AppError::new(
                ErrorCode::OfflineMode,
                "Network diagnostics are disabled by `--offline`.",
            ));
        }
        let connected = tokio::time::timeout(
            config.connect_timeout,
            TcpStream::connect(("api.figma.com", 443)),
        )
        .await
        .is_ok_and(|connection| connection.is_ok());
        checks.push(json!({
            "name": "figma_tcp_connectivity",
            "ok": connected,
            "endpoint": "api.figma.com:443",
            "endpointClass": null,
            "tier": null,
            "figmaRestRequests": 0
        }));
    }
    serialize_neutral(json!({
        "healthy": checks.iter().all(|check| check["ok"].as_bool().unwrap_or(false)),
        "checks": checks
    }))
}

async fn snapshot_pull(
    config: &AppConfig,
    offline: bool,
    args: &crate::args::SnapshotPullArgs,
) -> AppResult<CommandOutput> {
    let target = parse_figma_target(&args.target)?;
    let file_key = target.effective_file_key();
    let profile = request_profile(args.geometry);
    if offline {
        return Err(AppError::new(
            ErrorCode::OfflineMode,
            "Snapshot pull is disabled by `--offline`.",
        ));
    }
    let store = Store::open(&config.data_dir)?;
    let _lock = store.try_lock(file_key, profile.key())?;
    let existing = match store.resolve_snapshot(SnapshotSelector {
        file_key,
        request_profile: profile.key(),
        snapshot_id: None,
    }) {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            if error.code() == ErrorCode::SnapshotMissing {
                None
            } else {
                return Err(error);
            }
        }
    };
    let credential = PatProvider::new(SystemKeyring).resolve()?;
    let transport = ReqwestTransport::new(
        config.connect_timeout,
        config.total_timeout,
        config.inherit_proxy,
    )?;
    let gateway = FigmaGateway::new(transport, store.clone(), config.maximum_download_bytes);
    refresh_or_download(
        &store,
        &gateway,
        &target,
        profile,
        existing.as_ref(),
        args.force,
        args.version.as_deref(),
        &credential,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn refresh_or_download<T, L>(
    store: &Store,
    gateway: &FigmaGateway<T, L>,
    target: &FigmaTarget,
    profile: RequestProfile,
    existing: Option<&SnapshotSummary>,
    force: bool,
    version: Option<&str>,
    credential: &Credential,
) -> AppResult<CommandOutput>
where
    T: FigmaTransport,
    L: AttemptRecorder,
{
    if let Some(existing) = existing.filter(|_| !force && version.is_none()) {
        let metadata_stage = store.create_staging_file()?;
        let metadata_result = gateway
            .get_file_metadata(
                target.effective_file_key(),
                profile,
                credential,
                &metadata_stage,
            )
            .await;
        cleanup_stage(&metadata_stage);
        let metadata = metadata_result.map_err(|error| {
            if error.code() == ErrorCode::MetadataUnavailable {
                error
                    .with_detail(
                        "suggestedCommand",
                        json!(format!(
                            "figstash snapshot pull {} --force",
                            target.effective_file_key()
                        )),
                    )
                    .with_detail("tier1Attempted", json!(0))
            } else {
                error
            }
        })?;
        if metadata.metadata.version == existing.figma_version {
            return Ok(CommandOutput {
                data: json!({
                    "status": "unchanged",
                    "snapshot": existing,
                    "target": target,
                    "responseHash": existing.raw_blob_hash,
                    "responseSize": existing.raw_size
                }),
                source: ResponseSource::Mixed,
                snapshot_id: Some(existing.id.clone()),
                figma_version: Some(existing.figma_version.clone()),
                network: metadata.network,
                warnings: Vec::new(),
            });
        }
        return download_and_commit(
            store,
            gateway,
            target,
            profile,
            None,
            credential,
            metadata.network,
        )
        .await;
    }
    download_and_commit(
        store,
        gateway,
        target,
        profile,
        version,
        credential,
        NetworkUsage::default(),
    )
    .await
}

async fn download_and_commit<T, L>(
    store: &Store,
    gateway: &FigmaGateway<T, L>,
    target: &FigmaTarget,
    profile: RequestProfile,
    version: Option<&str>,
    credential: &Credential,
    prior_network: NetworkUsage,
) -> AppResult<CommandOutput>
where
    T: FigmaTransport,
    L: AttemptRecorder,
{
    let file_key = target.effective_file_key();
    let stage = store
        .create_staging_file()
        .map_err(|error| error.with_detail("network", json!(prior_network)))?;
    let receipt = match gateway
        .get_file(file_key, version, profile, credential, &stage)
        .await
    {
        Ok(receipt) => receipt,
        Err(error) => {
            cleanup_stage(&stage);
            return Err(with_prior_network(error, prior_network));
        }
    };
    let network = prior_network.combined(receipt.network);
    let bytes = fs::read(&stage).map_err(|error| {
        AppError::new(
            ErrorCode::StoreFailed,
            "Failed to read the completed staging response.",
        )
        .with_detail("reason", json!(error.to_string()))
        .with_detail("network", json!(network))
    })?;
    let actual_hash = blake3::hash(&bytes).to_hex().to_string();
    if actual_hash != receipt.hash || u64::try_from(bytes.len()).ok() != Some(receipt.size) {
        cleanup_stage(&stage);
        return Err(AppError::new(
            ErrorCode::StoreCorrupt,
            "The staged response changed after download.",
        )
        .with_detail("network", json!(network)));
    }
    let indexed = parse_file(&bytes).map_err(|error| {
        error
            .with_detail("network", json!(network))
            .with_detail("responseHash", json!(receipt.hash))
    })?;
    let warnings = indexed.warnings.clone();
    let summary = store
        .commit_snapshot(file_key, profile.key(), &stage, &indexed)
        .map_err(|error| error.with_detail("network", json!(network)))?;
    Ok(CommandOutput {
        data: json!({
            "status": "pulled",
            "snapshot": summary,
            "target": target,
            "responseHash": receipt.hash,
            "responseSize": receipt.size
        }),
        source: ResponseSource::Figma,
        snapshot_id: Some(summary.id.clone()),
        figma_version: Some(summary.figma_version.clone()),
        network,
        warnings,
    })
}

fn node_get(config: &AppConfig, args: &crate::args::NodeGetArgs) -> AppResult<CommandOutput> {
    let (target, node_id) = target_and_node(&args.target, args.node.as_deref())?;
    let profile = request_profile(args.geometry);
    let store = Store::open(&config.data_dir)?;
    let data = QueryService::new(store).node_get(NodeGetOptions {
        selector: SnapshotSelector {
            file_key: target.effective_file_key(),
            request_profile: profile.key(),
            snapshot_id: args.snapshot.as_deref(),
        },
        node_id: node_id.as_deref(),
        depth: args.depth,
        view: match args.view {
            ViewArg::Compact => View::Compact,
            ViewArg::Raw => View::Raw,
        },
    })?;
    local_with_snapshot(&data, &data.snapshot)
}

fn target_and_node(
    target: &str,
    explicit_node: Option<&str>,
) -> AppResult<(FigmaTarget, Option<String>)> {
    let target = parse_figma_target(target)?;
    let explicit_node = explicit_node.map(normalize_node_id).transpose()?;
    if let (Some(url_node), Some(explicit)) = (&target.node_id, &explicit_node) {
        if url_node != explicit {
            return Err(AppError::new(
                ErrorCode::InvalidArguments,
                "The URL node ID and `--node` identify different nodes.",
            )
            .with_details(json!({"urlNode": url_node, "explicitNode": explicit})));
        }
    }
    let node_id = explicit_node.or_else(|| target.node_id.clone());
    Ok((target, node_id))
}

fn node_search(config: &AppConfig, args: &crate::args::NodeSearchArgs) -> AppResult<CommandOutput> {
    if args.name.is_none() && args.text.is_none() && args.node_type.is_none() {
        return Err(AppError::new(
            ErrorCode::InvalidArguments,
            "Node search requires at least one of `--name`, `--text`, or `--type`.",
        ));
    }
    let target = parse_figma_target(&args.target)?;
    let ancestor = args
        .ancestor
        .as_deref()
        .map(normalize_node_id)
        .transpose()?;
    let profile = request_profile(args.geometry);
    let store = Store::open(&config.data_dir)?;
    let data = QueryService::new(store).node_search(
        SnapshotSelector {
            file_key: target.effective_file_key(),
            request_profile: profile.key(),
            snapshot_id: args.snapshot.as_deref(),
        },
        &figstash_core::NodeSearchQuery {
            name: args.name.clone(),
            text: args.text.clone(),
            node_type: args.node_type.clone(),
            ancestor_id: ancestor,
            limit: args.limit,
            cursor: args.cursor.clone(),
        },
    )?;
    local_with_snapshot(&data, &data.snapshot)
}

fn request_profile(geometry: GeometryArg) -> RequestProfile {
    RequestProfile {
        geometry: match geometry {
            GeometryArg::None => GeometryMode::None,
            GeometryArg::Paths => GeometryMode::Paths,
        },
    }
}

fn local_with_snapshot(
    data: impl serde::Serialize,
    snapshot: &figstash_core::SnapshotSummary,
) -> AppResult<CommandOutput> {
    let mut output = serialize_local(data)?;
    output.snapshot_id = Some(snapshot.id.clone());
    output.figma_version = Some(snapshot.figma_version.clone());
    Ok(output)
}

fn serialize_local(data: impl serde::Serialize) -> AppResult<CommandOutput> {
    CommandOutput::local(data).map_err(|error| serialization_error(&error))
}

fn serialize_neutral(data: impl serde::Serialize) -> AppResult<CommandOutput> {
    CommandOutput::neutral(data).map_err(|error| serialization_error(&error))
}

fn serialize_figma(
    data: impl serde::Serialize,
    network: figstash_core::NetworkUsage,
) -> AppResult<CommandOutput> {
    CommandOutput::figma(data, network).map_err(|error| serialization_error(&error))
}

fn serialization_error(error: &serde_json::Error) -> AppError {
    AppError::new(ErrorCode::Internal, "Failed to serialize command output.")
        .with_detail("reason", json!(error.to_string()))
}

fn cleanup_stage(path: &Path) {
    let _ = fs::remove_file(path);
}

fn with_prior_network(error: AppError, prior_network: NetworkUsage) -> AppError {
    let current = error
        .details()
        .get("network")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    error.with_detail("network", json!(prior_network.combined(current)))
}

fn initialize_logging(level: &str) {
    if level == "off" {
        return;
    }
    if let Ok(filter) = EnvFilter::try_new(level) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .try_init();
    }
}

fn chrono_like_age(timestamp: i64) -> Option<std::time::Duration> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let timestamp = u64::try_from(timestamp).ok()?;
    now.checked_sub(std::time::Duration::from_secs(timestamp))
}

#[cfg(test)]
mod tests {
    use super::{download_and_commit, refresh_or_download, whoami_with_gateway};
    use async_trait::async_trait;
    use figstash_core::{
        CredentialKind, EndpointClass, ErrorCode, NetworkUsage, RequestProfile, ResponseSource,
        SnapshotRepository, SnapshotSelector, SnapshotSummary,
    };
    use figstash_figma::{
        Credential, CredentialSource, FigmaGateway, FigmaTarget, FigmaTransport, RateLimitHeaders,
        TransportError, TransportResponse,
    };
    use figstash_query::{NodeGetOptions, QueryService, View};
    use figstash_store::Store;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use url::Url;

    struct FixtureTransport;

    struct CurrentUserTransport;

    struct RefreshTransport {
        metadata_status: u16,
        metadata_version: Option<String>,
        endpoints: Arc<Mutex<Vec<EndpointClass>>>,
    }

    #[async_trait]
    impl FigmaTransport for FixtureTransport {
        async fn download(
            &self,
            _url: &Url,
            _endpoint: figstash_core::EndpointClass,
            credential: &Credential,
            destination: &Path,
            _maximum_bytes: u64,
        ) -> Result<TransportResponse, TransportError> {
            assert_eq!(credential.kind(), CredentialKind::Pat);
            let bytes = include_bytes!("../../../fixtures/figma/feature-parity.json");
            tokio::fs::write(destination, bytes)
                .await
                .map_err(|error| {
                    TransportError::new(figstash_figma::TransportErrorKind::Io, error.to_string())
                })?;
            Ok(TransportResponse {
                status: 200,
                rate_limit: RateLimitHeaders::default(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                hash: blake3::hash(bytes).to_hex().to_string(),
            })
        }
    }

    #[async_trait]
    impl FigmaTransport for CurrentUserTransport {
        async fn download(
            &self,
            url: &Url,
            endpoint: figstash_core::EndpointClass,
            credential: &Credential,
            destination: &Path,
            _maximum_bytes: u64,
        ) -> Result<TransportResponse, TransportError> {
            assert_eq!(url.as_str(), "https://api.figma.com/v1/me");
            assert_eq!(endpoint, figstash_core::EndpointClass::GetCurrentUser);
            assert_eq!(credential.kind(), CredentialKind::Pat);
            let bytes = br#"{"id":"42","handle":"Agent User","email":"agent@example.com","img_url":"https://example.com/avatar.png"}"#;
            tokio::fs::write(destination, bytes)
                .await
                .map_err(|error| {
                    TransportError::new(figstash_figma::TransportErrorKind::Io, error.to_string())
                })?;
            Ok(TransportResponse {
                status: 200,
                rate_limit: RateLimitHeaders::default(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                hash: blake3::hash(bytes).to_hex().to_string(),
            })
        }
    }

    #[async_trait]
    impl FigmaTransport for RefreshTransport {
        async fn download(
            &self,
            url: &Url,
            endpoint: EndpointClass,
            credential: &Credential,
            destination: &Path,
            _maximum_bytes: u64,
        ) -> Result<TransportResponse, TransportError> {
            assert_eq!(credential.kind(), CredentialKind::Pat);
            self.endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}"))
                .push(endpoint);
            let (status, bytes) = match endpoint {
                EndpointClass::GetFileMeta => {
                    assert_eq!(
                        url.as_str(),
                        "https://api.figma.com/v1/files/SyntheticFileKey123/meta"
                    );
                    let body = self.metadata_version.as_ref().map_or_else(
                        || serde_json::json!({"file": {}}),
                        |version| serde_json::json!({"file": {"version": version}}),
                    );
                    (
                        self.metadata_status,
                        serde_json::to_vec(&body).unwrap_or_else(|error| {
                            panic!("metadata fixture serialization failed: {error}")
                        }),
                    )
                }
                EndpointClass::GetFile => {
                    assert_eq!(
                        url.as_str(),
                        "https://api.figma.com/v1/files/SyntheticFileKey123"
                    );
                    let mut body: serde_json::Value = serde_json::from_slice(include_bytes!(
                        "../../../fixtures/figma/feature-parity.json"
                    ))
                    .unwrap_or_else(|error| panic!("file fixture parse failed: {error}"));
                    if let Some(version) = &self.metadata_version {
                        body["version"] = serde_json::Value::String(version.clone());
                    }
                    (
                        200,
                        serde_json::to_vec(&body).unwrap_or_else(|error| {
                            panic!("file fixture serialization failed: {error}")
                        }),
                    )
                }
                other => panic!("unexpected refresh endpoint: {other:?}"),
            };
            tokio::fs::write(destination, &bytes)
                .await
                .map_err(|error| {
                    TransportError::new(figstash_figma::TransportErrorKind::Io, error.to_string())
                })?;
            Ok(TransportResponse {
                status,
                rate_limit: RateLimitHeaders::default(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                hash: blake3::hash(&bytes).to_hex().to_string(),
            })
        }
    }

    async fn prepared_refresh_state() -> (tempfile::TempDir, Store, FigmaTarget, SnapshotSummary) {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let target = FigmaTarget {
            file_key: "SyntheticFileKey123".to_owned(),
            branch_key: None,
            node_id: None,
        };
        let credential = Credential::personal_access_token("fixture", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        let gateway = FigmaGateway::new(FixtureTransport, store.clone(), 10 * 1024 * 1024);
        let output = download_and_commit(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            None,
            &credential,
            NetworkUsage::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("initial mock pull failed: {error}"));
        let snapshot = store
            .resolve_snapshot(SnapshotSelector {
                file_key: target.effective_file_key(),
                request_profile: RequestProfile::default().key(),
                snapshot_id: output.snapshot_id.as_deref(),
            })
            .unwrap_or_else(|error| panic!("prepared snapshot lookup failed: {error}"));
        (temporary, store, target, snapshot)
    }

    fn refresh_transport(
        metadata_status: u16,
        metadata_version: Option<&str>,
    ) -> (RefreshTransport, Arc<Mutex<Vec<EndpointClass>>>) {
        let endpoints = Arc::new(Mutex::new(Vec::new()));
        (
            RefreshTransport {
                metadata_status,
                metadata_version: metadata_version.map(ToOwned::to_owned),
                endpoints: Arc::clone(&endpoints),
            },
            endpoints,
        )
    }

    fn fixture_credential() -> Credential {
        Credential::personal_access_token("fixture", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"))
    }

    #[tokio::test]
    async fn unchanged_refresh_spends_one_tier_three_and_zero_tier_one() {
        let (_temporary, store, target, existing) = prepared_refresh_state().await;
        let (transport, endpoints) = refresh_transport(200, Some("1001"));
        let gateway = FigmaGateway::new(transport, store.clone(), 10 * 1024 * 1024);
        let output = refresh_or_download(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            Some(&existing),
            false,
            None,
            &fixture_credential(),
        )
        .await
        .unwrap_or_else(|error| panic!("unchanged refresh failed: {error}"));

        assert_eq!(output.data["status"], "unchanged");
        assert_eq!(output.source, ResponseSource::Mixed);
        assert_eq!(output.snapshot_id.as_deref(), Some(existing.id.as_str()));
        assert_eq!(output.network.attempts, 1);
        assert_eq!(output.network.tier1, 0);
        assert_eq!(output.network.tier3, 1);
        assert_eq!(
            *endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}")),
            vec![EndpointClass::GetFileMeta]
        );
    }

    #[tokio::test]
    async fn changed_refresh_spends_tier_three_then_one_tier_one() {
        let (_temporary, store, target, existing) = prepared_refresh_state().await;
        let (transport, endpoints) = refresh_transport(200, Some("1002"));
        let gateway = FigmaGateway::new(transport, store.clone(), 10 * 1024 * 1024);
        let output = refresh_or_download(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            Some(&existing),
            false,
            None,
            &fixture_credential(),
        )
        .await
        .unwrap_or_else(|error| panic!("changed refresh failed: {error}"));

        assert_eq!(output.data["status"], "pulled");
        assert_eq!(output.figma_version.as_deref(), Some("1002"));
        assert_eq!(output.network.attempts, 2);
        assert_eq!(output.network.tier1, 1);
        assert_eq!(output.network.tier3, 1);
        assert_eq!(
            *endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}")),
            vec![EndpointClass::GetFileMeta, EndpointClass::GetFile]
        );
    }

    #[tokio::test]
    async fn missing_metadata_scope_fails_closed_without_tier_one() {
        let (_temporary, store, target, existing) = prepared_refresh_state().await;
        let (transport, endpoints) = refresh_transport(403, None);
        let gateway = FigmaGateway::new(transport, store.clone(), 10 * 1024 * 1024);
        let result = refresh_or_download(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            Some(&existing),
            false,
            None,
            &fixture_credential(),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("missing metadata scope must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), ErrorCode::MetadataUnavailable);
        assert_eq!(error.details()["tier1Attempted"], 0);
        assert_eq!(
            error.details()["suggestedCommand"],
            "figstash snapshot pull SyntheticFileKey123 --force"
        );
        assert_eq!(error.details()["network"]["attempts"], 1);
        assert_eq!(error.details()["network"]["tier1"], 0);
        assert_eq!(error.details()["network"]["tier3"], 1);
        assert_eq!(
            *endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}")),
            vec![EndpointClass::GetFileMeta]
        );
    }

    #[tokio::test]
    async fn missing_metadata_version_fails_closed_without_tier_one() {
        let (_temporary, store, target, existing) = prepared_refresh_state().await;
        let (transport, endpoints) = refresh_transport(200, None);
        let gateway = FigmaGateway::new(transport, store.clone(), 10 * 1024 * 1024);
        let result = refresh_or_download(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            Some(&existing),
            false,
            None,
            &fixture_credential(),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("missing metadata version must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), ErrorCode::InvalidFigmaResponse);
        assert_eq!(error.details()["network"]["attempts"], 1);
        assert_eq!(error.details()["network"]["tier1"], 0);
        assert_eq!(error.details()["network"]["tier3"], 1);
        assert_eq!(
            *endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}")),
            vec![EndpointClass::GetFileMeta]
        );
    }

    #[tokio::test]
    async fn force_refresh_skips_metadata_and_spends_one_tier_one() {
        let (_temporary, store, target, existing) = prepared_refresh_state().await;
        let (transport, endpoints) = refresh_transport(200, Some("1002"));
        let gateway = FigmaGateway::new(transport, store.clone(), 10 * 1024 * 1024);
        let output = refresh_or_download(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            Some(&existing),
            true,
            None,
            &fixture_credential(),
        )
        .await
        .unwrap_or_else(|error| panic!("force refresh failed: {error}"));

        assert_eq!(output.network.attempts, 1);
        assert_eq!(output.network.tier1, 1);
        assert_eq!(output.network.tier3, 0);
        assert_eq!(
            *endpoints
                .lock()
                .unwrap_or_else(|error| panic!("endpoint mutex poisoned: {error}")),
            vec![EndpointClass::GetFile]
        );
    }

    #[tokio::test]
    async fn mock_whoami_returns_normalized_identity_and_records_tier_three() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let stage = store
            .create_staging_file()
            .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
        let gateway = FigmaGateway::new(CurrentUserTransport, store.clone(), 64 * 1024);
        let credential = Credential::personal_access_token("fixture", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        let output = whoami_with_gateway(&gateway, &credential, &stage)
            .await
            .unwrap_or_else(|error| panic!("mock whoami failed: {error}"));
        assert_eq!(output.source, figstash_core::ResponseSource::Figma);
        assert_eq!(output.network.attempts, 1);
        assert_eq!(output.network.tier3, 1);
        assert_eq!(output.data["credentialKind"], "pat");
        assert_eq!(output.data["credentialSource"], "keyring");
        assert_eq!(output.data["remoteValidated"], true);
        assert_eq!(output.data["user"]["id"], "42");
        assert_eq!(output.data["user"]["handle"], "Agent User");
        assert_eq!(
            output.data["user"]["avatarUrl"],
            "https://example.com/avatar.png"
        );
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../schemas/cli/v1/auth.whoami.schema.json"
        ))
        .unwrap_or_else(|error| panic!("whoami schema is invalid: {error}"));
        let validator = jsonschema::validator_for(&schema)
            .unwrap_or_else(|error| panic!("whoami schema compilation failed: {error}"));
        validator
            .validate(&output.data)
            .unwrap_or_else(|error| panic!("whoami output violates its schema: {error}"));
        let quota = store
            .quota_status()
            .unwrap_or_else(|error| panic!("quota lookup failed: {error}"));
        assert_eq!(quota.attempted, 1);
        assert_eq!(quota.succeeded, 1);
    }

    #[tokio::test]
    async fn mock_pull_spends_one_tier_one_then_all_queries_are_local() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let gateway = FigmaGateway::new(FixtureTransport, store.clone(), 10 * 1024 * 1024);
        let credential = Credential::personal_access_token("fixture", CredentialSource::Keyring)
            .unwrap_or_else(|error| panic!("credential failed: {error}"));
        let target = FigmaTarget {
            file_key: "SyntheticFileKey123".to_owned(),
            branch_key: None,
            node_id: None,
        };
        let output = download_and_commit(
            &store,
            &gateway,
            &target,
            RequestProfile::default(),
            None,
            &credential,
            NetworkUsage::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("mock pull failed: {error}"));
        assert_eq!(output.network.attempts, 1);
        assert_eq!(output.network.tier1, 1);

        let summary = store
            .resolve_snapshot(SnapshotSelector {
                file_key: target.effective_file_key(),
                request_profile: RequestProfile::default().key(),
                snapshot_id: None,
            })
            .unwrap_or_else(|error| panic!("snapshot lookup failed: {error}"));
        let node = QueryService::new(store.clone())
            .node_get(NodeGetOptions {
                selector: SnapshotSelector {
                    file_key: target.effective_file_key(),
                    request_profile: RequestProfile::default().key(),
                    snapshot_id: Some(&summary.id),
                },
                node_id: Some("2:2"),
                depth: Some(0),
                view: View::Compact,
            })
            .unwrap_or_else(|error| panic!("local query failed: {error}"));
        assert_eq!(node.node["id"], "2:2");
        let quota = store
            .quota_status()
            .unwrap_or_else(|error| panic!("quota lookup failed: {error}"));
        assert_eq!(quota.attempted, 1);
        assert_eq!(quota.succeeded, 1);
    }
}
