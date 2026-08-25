//! `SQLite` catalog and atomic snapshot transactions.

use crate::blob::BlobStore;
use crate::error::{corrupt_error, io_error, sql_error};
use crate::models::{
    ApiAttemptRow, ComponentRow, ComponentSetRow, DerivedArtifactRow, HeadRow, NodeRow,
    SchemaMigrationRow, SnapshotRow, StyleRow,
};
use chrono::{DateTime, Datelike, Utc};
use figstash_core::{
    ApiAttempt, AppError, AppResult, AttemptRecorder, BoundingBox, ComponentUsage, EndpointClass,
    ErrorCode, IndexedEntity, IndexedNode, IndexedSnapshot, NodeSearchQuery, NodeSearchResult,
    PARSER_VERSION, SnapshotDiffNode, SnapshotRepository, SnapshotSelector, SnapshotSummary,
    StoredNode, Tier,
};
use fs2::FileExt;
use serde::Serialize;
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use toasty::{Db, Executor, stmt::Query};
use uuid::Uuid;

const STORE_VERSION: &str = "store-v1";
const MIGRATION_VERSION: i64 = 1;
const MIGRATION_SQL: &str = r"
CREATE TABLE IF NOT EXISTS snapshots(
  id TEXT PRIMARY KEY,
  file_key TEXT NOT NULL,
  figma_version TEXT NOT NULL,
  request_profile TEXT NOT NULL,
  file_name TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  last_modified TEXT,
  raw_blob_hash TEXT NOT NULL,
  raw_size INTEGER NOT NULL,
  node_count INTEGER NOT NULL,
  parser_version INTEGER NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('ready'))
);
CREATE INDEX IF NOT EXISTS snapshots_lineage ON snapshots(file_key, request_profile, fetched_at DESC);
CREATE TABLE IF NOT EXISTS heads(
  file_key TEXT NOT NULL,
  request_profile TEXT NOT NULL,
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE RESTRICT,
  PRIMARY KEY(file_key, request_profile)
);
CREATE TABLE IF NOT EXISTS nodes(
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
  node_id TEXT NOT NULL,
  parent_id TEXT,
  node_type TEXT NOT NULL,
  name TEXT NOT NULL,
  depth INTEGER NOT NULL,
  sibling_order INTEGER NOT NULL,
  path_ids TEXT NOT NULL,
  visible INTEGER,
  x REAL, y REAL, width REAL, height REAL,
  component_id TEXT,
  text_content TEXT,
  node_blob_hash TEXT NOT NULL,
  subtree_hash TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, node_id)
);
CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(snapshot_id, parent_id, sibling_order);
CREATE INDEX IF NOT EXISTS nodes_component ON nodes(snapshot_id, component_id);
CREATE VIRTUAL TABLE IF NOT EXISTS node_fts USING fts5(
  snapshot_id UNINDEXED, node_id UNINDEXED, name, text_content
);
CREATE TABLE IF NOT EXISTS styles(
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
  style_id TEXT NOT NULL,
  style_type TEXT NOT NULL,
  name TEXT,
  json_blob_hash TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, style_id)
);
CREATE TABLE IF NOT EXISTS components(
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
  component_id TEXT NOT NULL,
  node_id TEXT,
  name TEXT,
  json_blob_hash TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, component_id)
);
CREATE TABLE IF NOT EXISTS component_sets(
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
  component_set_id TEXT NOT NULL,
  node_id TEXT,
  name TEXT,
  json_blob_hash TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, component_set_id)
);
CREATE TABLE IF NOT EXISTS derived_artifacts(
  snapshot_id TEXT NOT NULL REFERENCES snapshots(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  blob_hash TEXT NOT NULL,
  PRIMARY KEY(snapshot_id, kind, schema_version)
);
CREATE TABLE IF NOT EXISTS api_attempts(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_at TEXT NOT NULL,
  completed_at TEXT NOT NULL,
  command TEXT NOT NULL,
  endpoint_class TEXT NOT NULL,
  tier TEXT NOT NULL,
  file_key TEXT,
  request_profile TEXT,
  http_status INTEGER,
  error_code TEXT,
  retry_after INTEGER,
  plan_tier TEXT,
  rate_limit_type TEXT
);
CREATE TABLE IF NOT EXISTS schema_migrations(
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL,
  checksum TEXT NOT NULL
);
";

/// Durable local Figstash storage rooted in an explicitly resolved data directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    catalog_path: PathBuf,
    blobs: BlobStore,
    db: Db,
}

/// Held exclusive writer lock for one file/request-profile lineage.
#[derive(Debug)]
pub struct WriterLock {
    file: File,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Safe deletion plan produced before an explicit prune execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrunePlan {
    /// Immutable snapshots eligible for deletion.
    pub snapshot_ids: Vec<String>,
    /// Content-addressed blobs that would become unreferenced.
    pub blob_hashes: Vec<String>,
    /// Total number of snapshots retained, including all lineage heads.
    pub retained_snapshots: u64,
    /// Whether this value describes an executed deletion.
    pub executed: bool,
    /// Deletions are permanent unless the data directory is externally backed up.
    pub recoverable: bool,
}

/// Aggregated locally observed request count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaBucket {
    /// Grouping key such as tier, endpoint, file, or command.
    pub key: String,
    /// Attempted request count.
    pub attempted: u64,
    /// Responses with a successful HTTP status.
    pub succeeded: u64,
}

/// Most recent locally observed Figma rate limit response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentRateLimit {
    /// Attempt completion time.
    pub completed_at: String,
    /// Retry delay reported by Figma.
    pub retry_after: Option<u64>,
    /// Plan tier header reported by Figma.
    pub plan_tier: Option<String>,
    /// Rate limit type header reported by Figma.
    pub rate_limit_type: Option<String>,
}

/// Current-month request ledger summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatus {
    /// Explicitly limits the report to requests observed by this installation.
    pub scope: String,
    /// UTC calendar month in `YYYY-MM` form.
    pub month: String,
    /// Total attempted requests.
    pub attempted: u64,
    /// Total successful requests.
    pub succeeded: u64,
    /// Counts grouped by quota tier.
    pub by_tier: Vec<QuotaBucket>,
    /// Counts grouped by endpoint class.
    pub by_endpoint: Vec<QuotaBucket>,
    /// Counts grouped by canonical command.
    pub by_command: Vec<QuotaBucket>,
    /// Counts grouped by file key.
    pub by_file: Vec<QuotaBucket>,
    /// Most recent rate-limit response, if observed.
    pub recent_rate_limit: Option<RecentRateLimit>,
    /// Primary source for the built-in policy.
    pub policy_source: String,
    /// Date on which the built-in policy was verified.
    pub policy_updated_at: String,
}

impl Store {
    /// Opens or initializes `store-v1` below the supplied durable data directory.
    ///
    /// # Errors
    ///
    /// Returns a storage or migration-integrity error when initialization fails.
    pub async fn open(data_directory: impl AsRef<Path>) -> AppResult<Self> {
        let root = data_directory.as_ref().join(STORE_VERSION);
        for directory in [
            root.clone(),
            root.join("blobs/b3"),
            root.join("staging"),
            root.join("locks"),
            root.join("backups"),
        ] {
            fs::create_dir_all(&directory).map_err(|error| {
                io_error("Failed to create the Figstash data directory.", error)
            })?;
            restrict_directory(&directory)?;
        }
        let catalog_path = root.join("catalog.sqlite3");
        let mut builder = Db::builder();
        builder
            .models(toasty::models!(
                SnapshotRow,
                HeadRow,
                NodeRow,
                StyleRow,
                ComponentRow,
                ComponentSetRow,
                DerivedArtifactRow,
                ApiAttemptRow,
                SchemaMigrationRow
            ))
            .max_pool_size(1);
        let database_url = format!("sqlite:{}", catalog_path.display());
        let db = builder
            .connect(&database_url)
            .await
            .map_err(|error| sql_error("Failed to open the snapshot catalog.", error))?;
        let store = Self {
            blobs: BlobStore::new(root.join("blobs")),
            root,
            catalog_path,
            db,
        };
        store.initialize_catalog().await?;
        store.cleanup_orphan_staging(Duration::from_hours(24))?;
        Ok(store)
    }

    /// Returns the versioned store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Allocates an empty uniquely named staging response file.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the staging file cannot be created securely.
    pub fn create_staging_file(&self) -> AppResult<PathBuf> {
        let path = self
            .root
            .join("staging")
            .join(format!("{}.json.part", Uuid::new_v4()));
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| io_error("Failed to allocate a staging file.", error))?;
        restrict_file(&path)?;
        Ok(path)
    }

    /// Acquires a non-blocking cross-process writer lock for one lineage.
    ///
    /// # Errors
    ///
    /// Returns `store_busy` when held elsewhere, or a storage error on file failure.
    pub fn try_lock(&self, file_key: &str, request_profile: &str) -> AppResult<WriterLock> {
        let lock_name = blake3::hash(format!("{file_key}\0{request_profile}").as_bytes())
            .to_hex()
            .to_string();
        let path = self.root.join("locks").join(format!("{lock_name}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| io_error("Failed to open the snapshot writer lock.", error))?;
        restrict_file(&path)?;
        file.try_lock_exclusive().map_err(|error| {
            AppError::new(
                ErrorCode::StoreBusy,
                "Another process is already refreshing this file and request profile.",
            )
            .with_details(json!({
                "fileKey": file_key,
                "requestProfile": request_profile,
                "reason": error.to_string(),
            }))
            .retryable(true)
        })?;
        Ok(WriterLock { file })
    }

    /// Atomically commits an indexed immutable snapshot and advances its HEAD.
    ///
    /// # Errors
    ///
    /// Returns an integrity or storage error. A failed transaction never moves `HEAD`.
    pub async fn commit_snapshot(
        &self,
        file_key: &str,
        request_profile: &str,
        staged_response: &Path,
        indexed: &IndexedSnapshot,
    ) -> AppResult<SnapshotSummary> {
        let (raw_blob_hash, raw_size) = self.blobs.put_file(staged_response)?;
        let snapshot_seed = format!(
            "{file_key}\0{request_profile}\0{}\0{raw_blob_hash}\0{PARSER_VERSION}",
            indexed.figma_version
        );
        let snapshot_id = format!("b3:{}", blake3::hash(snapshot_seed.as_bytes()).to_hex());
        let fetched_at = Utc::now();
        let node_count = u64::try_from(indexed.nodes.len()).map_err(|error| {
            corrupt_error("The indexed node count cannot be represented.", error)
        })?;
        let summary = SnapshotSummary {
            id: snapshot_id,
            file_key: file_key.to_owned(),
            figma_version: indexed.figma_version.clone(),
            request_profile: request_profile.to_owned(),
            file_name: indexed.file_name.clone(),
            fetched_at,
            last_modified: indexed.last_modified.clone(),
            raw_blob_hash,
            raw_size,
            node_count,
            parser_version: PARSER_VERSION,
            is_head: true,
        };

        let mut db = self.db.clone();
        let mut transaction = db
            .transaction()
            .await
            .map_err(|error| sql_error("Failed to start the snapshot transaction.", error))?;
        self.insert_snapshot(&mut transaction, &summary, indexed)
            .await?;
        toasty::sql::statement(
            "INSERT INTO heads(file_key, request_profile, snapshot_id) VALUES(?1, ?2, ?3)
             ON CONFLICT(file_key, request_profile) DO UPDATE SET snapshot_id=excluded.snapshot_id",
        )
        .bind(file_key)
        .bind(request_profile)
        .bind(summary.id.as_str())
        .exec(&mut transaction)
        .await
        .map_err(|error| sql_error("Failed to advance the snapshot HEAD.", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| sql_error("Failed to commit the snapshot transaction.", error))?;
        let _ = fs::remove_file(staged_response);
        self.resolve_snapshot(SnapshotSelector {
            file_key,
            request_profile,
            snapshot_id: Some(&summary.id),
        })
        .await
    }

    /// Lists immutable snapshots newest first.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when catalog rows cannot be loaded.
    pub async fn list_snapshots(&self, file_key: Option<&str>) -> AppResult<Vec<SnapshotSummary>> {
        let mut query = Query::<toasty::stmt::List<SnapshotRow>>::all();
        if let Some(file_key) = file_key {
            query = query.filter(SnapshotRow::fields().file_key().eq(file_key));
        }
        let mut db = self.db.clone();
        let rows = query
            .order_by((
                SnapshotRow::fields().fetched_at().desc(),
                SnapshotRow::fields().id().asc(),
            ))
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to list snapshots.", error))?;
        let heads = Query::<toasty::stmt::List<HeadRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load snapshot heads.", error))?
            .into_iter()
            .map(|head| head.snapshot_id)
            .collect::<std::collections::BTreeSet<_>>();
        rows.into_iter()
            .map(|row| {
                let is_head = heads.contains(&row.id);
                snapshot_from_model(row, is_head)
            })
            .collect()
    }

    /// Creates a safe plan retaining every current HEAD and every lineage's sole snapshot.
    ///
    /// # Errors
    ///
    /// Returns a storage error when reachability cannot be calculated.
    pub async fn prune_plan(&self) -> AppResult<PrunePlan> {
        let mut db = self.db.clone();
        let mut snapshots = Query::<toasty::stmt::List<SnapshotRow>>::all()
            .order_by((
                SnapshotRow::fields().fetched_at().asc(),
                SnapshotRow::fields().id().asc(),
            ))
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to build the prune plan.", error))?;
        let heads = Query::<toasty::stmt::List<HeadRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load snapshot heads.", error))?
            .into_iter()
            .map(|head| head.snapshot_id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut lineage_counts = std::collections::BTreeMap::new();
        for snapshot in &snapshots {
            *lineage_counts
                .entry((snapshot.file_key.clone(), snapshot.request_profile.clone()))
                .or_insert(0_usize) += 1;
        }
        let snapshot_ids = snapshots
            .drain(..)
            .filter(|snapshot| {
                !heads.contains(&snapshot.id)
                    && lineage_counts
                        .get(&(snapshot.file_key.clone(), snapshot.request_profile.clone()))
                        .is_some_and(|count| *count > 1)
            })
            .map(|snapshot| snapshot.id)
            .collect::<Vec<_>>();
        let blob_hashes = self.unreferenced_after(&snapshot_ids).await?;
        let total = Query::<toasty::stmt::List<SnapshotRow>>::all()
            .count()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to count retained snapshots.", error))?;
        let retained_snapshots =
            total.saturating_sub(u64::try_from(snapshot_ids.len()).unwrap_or(u64::MAX));
        Ok(PrunePlan {
            snapshot_ids,
            blob_hashes,
            retained_snapshots,
            executed: false,
            recoverable: false,
        })
    }

    /// Executes the current safe prune plan and removes newly unreferenced blobs.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the catalog transaction or blob collection fails.
    pub async fn execute_prune(&self) -> AppResult<PrunePlan> {
        let mut plan = self.prune_plan().await?;
        let mut db = self.db.clone();
        let mut transaction = db
            .transaction()
            .await
            .map_err(|error| sql_error("Failed to start the prune transaction.", error))?;
        for snapshot_id in &plan.snapshot_ids {
            toasty::sql::statement("DELETE FROM node_fts WHERE snapshot_id=?1")
                .bind(snapshot_id.as_str())
                .exec(&mut transaction)
                .await
                .map_err(|error| sql_error("Failed to prune node search rows.", error))?;
            Query::<toasty::stmt::List<SnapshotRow>>::all()
                .filter(SnapshotRow::fields().id().eq(snapshot_id.as_str()))
                .delete()
                .exec(&mut transaction)
                .await
                .map_err(|error| sql_error("Failed to prune a snapshot.", error))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| sql_error("Failed to commit the prune transaction.", error))?;
        for hash in &plan.blob_hashes {
            let path = self.blobs.path(hash);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("Failed to garbage-collect a blob.", error)),
            }
        }
        plan.executed = true;
        Ok(plan)
    }

    /// Returns the current UTC month's observed-only request ledger summary.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the local request ledger cannot be summarized.
    pub async fn quota_status(&self) -> AppResult<QuotaStatus> {
        let now = Utc::now();
        let month = format!("{:04}-{:02}", now.year(), now.month());
        let mut db = self.db.clone();
        let attempts = Query::<toasty::stmt::List<ApiAttemptRow>>::all()
            .filter(ApiAttemptRow::fields().completed_at().starts_with(&month))
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to summarize the request ledger.", error))?;
        let attempted = u64::try_from(attempts.len())
            .map_err(|error| corrupt_error("The request count cannot be represented.", error))?;
        let succeeded = u64::try_from(
            attempts
                .iter()
                .filter(|attempt| {
                    attempt
                        .http_status
                        .is_some_and(|status| (200..=299).contains(&status))
                })
                .count(),
        )
        .map_err(|error| {
            corrupt_error("The successful request count cannot be represented.", error)
        })?;
        Ok(QuotaStatus {
            scope: "figstash_observed_only".to_owned(),
            month,
            attempted,
            succeeded,
            by_tier: grouped_attempt_models(&attempts, |attempt| attempt.tier.as_str())?,
            by_endpoint: grouped_attempt_models(&attempts, |attempt| {
                attempt.endpoint_class.as_str()
            })?,
            by_command: grouped_attempt_models(&attempts, |attempt| attempt.command.as_str())?,
            by_file: grouped_attempt_models(&attempts, |attempt| {
                attempt.file_key.as_deref().unwrap_or("none")
            })?,
            recent_rate_limit: latest_rate_limit_model(&attempts)?,
            policy_source: "https://developers.figma.com/docs/rest-api/rate-limits/".to_owned(),
            policy_updated_at: "2026-08-20".to_owned(),
        })
    }

    /// Copies the closed-form catalog into the store backup directory or a supplied path.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the catalog cannot be copied or secured.
    pub fn backup_catalog(&self, destination: Option<&Path>) -> AppResult<PathBuf> {
        let destination = destination.map_or_else(
            || {
                self.root.join("backups").join(format!(
                    "catalog-{}.sqlite3",
                    Utc::now().format("%Y%m%dT%H%M%SZ")
                ))
            },
            Path::to_path_buf,
        );
        fs::copy(&self.catalog_path, &destination)
            .map_err(|error| io_error("Failed to back up the snapshot catalog.", error))?;
        restrict_file(&destination)?;
        Ok(destination)
    }

    async fn initialize_catalog(&self) -> AppResult<()> {
        let mut connection = self
            .db
            .connection()
            .await
            .map_err(|error| sql_error("Failed to acquire the snapshot catalog.", error))?;
        for pragma in [
            "PRAGMA foreign_keys=ON",
            "PRAGMA journal_mode=WAL",
            "PRAGMA synchronous=FULL",
            "PRAGMA busy_timeout=5000",
        ] {
            toasty::sql::query(pragma)
                .exec(&mut connection)
                .await
                .map_err(|error| sql_error("Failed to configure the snapshot catalog.", error))?;
        }
        let migration_table_exists = !toasty::sql::query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='schema_migrations'",
        )
        .exec(&mut connection)
        .await
        .map_err(|error| sql_error("Failed to inspect catalog migrations.", error))?
        .is_empty();
        let checksum = blake3::hash(MIGRATION_SQL.as_bytes()).to_hex().to_string();
        let existing = if migration_table_exists {
            let migrations = Query::<toasty::stmt::List<SchemaMigrationRow>>::all()
                .exec(&mut connection)
                .await
                .map_err(|error| sql_error("Failed to inspect catalog migrations.", error))?;
            if let Some(future) = migrations
                .iter()
                .find(|migration| migration.version > MIGRATION_VERSION)
            {
                return Err(corrupt_error(
                    "The catalog schema is newer than this Figstash binary.",
                    format!("unknown migration version {}", future.version),
                ));
            }
            migrations
                .into_iter()
                .find(|migration| migration.version == MIGRATION_VERSION)
                .map(|migration| migration.checksum)
        } else {
            None
        };
        if let Some(existing) = existing {
            if existing != checksum {
                return Err(corrupt_error(
                    "The catalog migration checksum does not match this binary.",
                    format!("version {MIGRATION_VERSION}"),
                ));
            }
        } else {
            drop(connection);
            let mut db = self.db.clone();
            let mut transaction = db
                .transaction()
                .await
                .map_err(|error| sql_error("Failed to start the catalog migration.", error))?;
            for statement in MIGRATION_SQL
                .split(';')
                .map(str::trim)
                .filter(|sql| !sql.is_empty())
            {
                toasty::sql::statement(statement)
                    .exec(&mut transaction)
                    .await
                    .map_err(|error| {
                        sql_error("Failed to initialize the snapshot catalog.", error)
                    })?;
            }
            toasty::create!(SchemaMigrationRow {
                version: MIGRATION_VERSION,
                applied_at: Utc::now().to_rfc3339(),
                checksum,
            })
            .exec(&mut transaction)
            .await
            .map_err(|error| sql_error("Failed to record the catalog migration.", error))?;
            transaction
                .commit()
                .await
                .map_err(|error| sql_error("Failed to commit the catalog migration.", error))?;
        }
        restrict_file(&self.catalog_path)
    }

    async fn insert_snapshot(
        &self,
        transaction: &mut dyn Executor,
        summary: &SnapshotSummary,
        indexed: &IndexedSnapshot,
    ) -> AppResult<()> {
        let raw_size = i64::try_from(summary.raw_size).map_err(|error| {
            corrupt_error("The raw snapshot size exceeds SQLite integer range.", error)
        })?;
        let node_count = i64::try_from(summary.node_count).map_err(|error| {
            corrupt_error("The node count exceeds SQLite integer range.", error)
        })?;
        let existing = Query::<toasty::stmt::List<SnapshotRow>>::all()
            .filter(SnapshotRow::fields().id().eq(summary.id.as_str()))
            .first()
            .exec(transaction)
            .await
            .map_err(|error| sql_error("Failed to inspect snapshot metadata.", error))?;
        if existing.is_none() {
            toasty::create!(SnapshotRow {
                id: summary.id.clone(),
                file_key: summary.file_key.clone(),
                figma_version: summary.figma_version.clone(),
                request_profile: summary.request_profile.clone(),
                file_name: summary.file_name.clone(),
                fetched_at: summary.fetched_at.to_rfc3339(),
                last_modified: summary.last_modified.clone(),
                raw_blob_hash: summary.raw_blob_hash.clone(),
                raw_size,
                node_count,
                parser_version: i64::from(summary.parser_version),
                status: "ready".to_owned(),
            })
            .exec(transaction)
            .await
            .map_err(|error| sql_error("Failed to insert snapshot metadata.", error))?;
        }
        let already_indexed = Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(NodeRow::fields().snapshot_id().eq(summary.id.as_str()))
            .count()
            .exec(transaction)
            .await
            .map_err(|error| sql_error("Failed to inspect existing snapshot indexes.", error))?;
        if already_indexed > 0 {
            return Ok(());
        }
        for node in &indexed.nodes {
            self.insert_node(transaction, &summary.id, node).await?;
        }
        self.insert_entities(transaction, "styles", &summary.id, &indexed.styles)
            .await?;
        self.insert_entities(transaction, "components", &summary.id, &indexed.components)
            .await?;
        self.insert_entities(
            transaction,
            "component_sets",
            &summary.id,
            &indexed.component_sets,
        )
        .await?;
        Ok(())
    }

    async fn insert_node(
        &self,
        transaction: &mut dyn Executor,
        snapshot_id: &str,
        node: &IndexedNode,
    ) -> AppResult<()> {
        let raw = serde_json::to_vec(&node.raw)
            .map_err(|error| corrupt_error("Failed to serialize an indexed node.", error))?;
        let blob_hash = self.blobs.put_bytes(&raw)?;
        let path_ids = serde_json::to_string(&node.path_ids)
            .map_err(|error| corrupt_error("Failed to serialize a node identifier path.", error))?;
        let bounds = node.bounds.unwrap_or(BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        let has_bounds = node.bounds.is_some();
        toasty::create!(NodeRow {
            snapshot_id: snapshot_id.to_owned(),
            node_id: node.node_id.clone(),
            parent_id: node.parent_id.clone(),
            node_type: node.node_type.clone(),
            name: node.name.clone(),
            depth: i64::from(node.depth),
            sibling_order: i64::from(node.sibling_order),
            path_ids,
            visible: node.visible,
            x: has_bounds.then_some(bounds.x),
            y: has_bounds.then_some(bounds.y),
            width: has_bounds.then_some(bounds.width),
            height: has_bounds.then_some(bounds.height),
            component_id: node.component_id.clone(),
            text_content: node.text_content.clone(),
            node_blob_hash: blob_hash,
            subtree_hash: node.subtree_hash.clone(),
        })
        .exec(transaction)
        .await
        .map_err(|error| sql_error("Failed to insert a node index row.", error))?;
        let mut statement = toasty::sql::statement(
            "INSERT INTO node_fts(snapshot_id,node_id,name,text_content) VALUES(?1,?2,?3,?4)",
        )
        .bind(snapshot_id)
        .bind(node.node_id.as_str())
        .bind(node.name.as_str());
        statement = match node.text_content.as_deref() {
            Some(text) => statement.bind(text),
            None => statement.bind_typed(toasty::stmt::Value::Null, toasty::schema::db::Type::Text),
        };
        statement
            .exec(transaction)
            .await
            .map_err(|error| sql_error("Failed to insert a node search row.", error))?;
        Ok(())
    }

    async fn insert_entities(
        &self,
        transaction: &mut dyn Executor,
        table: &str,
        snapshot_id: &str,
        entities: &[IndexedEntity],
    ) -> AppResult<()> {
        for entity in entities {
            let raw = serde_json::to_vec(&entity.raw)
                .map_err(|error| corrupt_error("Failed to serialize an indexed entity.", error))?;
            let blob_hash = self.blobs.put_bytes(&raw)?;
            match table {
                "styles" => {
                    toasty::create!(StyleRow {
                        snapshot_id: snapshot_id.to_owned(),
                        style_id: entity.id.clone(),
                        style_type: entity.entity_type.clone(),
                        name: entity.name.clone(),
                        json_blob_hash: blob_hash,
                    })
                    .exec(transaction)
                    .await
                    .map_err(|error| sql_error("Failed to insert an indexed style.", error))?;
                }
                "components" => {
                    toasty::create!(ComponentRow {
                        snapshot_id: snapshot_id.to_owned(),
                        component_id: entity.id.clone(),
                        node_id: entity.node_id.clone(),
                        name: entity.name.clone(),
                        json_blob_hash: blob_hash,
                    })
                    .exec(transaction)
                    .await
                    .map_err(|error| sql_error("Failed to insert an indexed component.", error))?;
                }
                "component_sets" => {
                    toasty::create!(ComponentSetRow {
                        snapshot_id: snapshot_id.to_owned(),
                        component_set_id: entity.id.clone(),
                        node_id: entity.node_id.clone(),
                        name: entity.name.clone(),
                        json_blob_hash: blob_hash,
                    })
                    .exec(transaction)
                    .await
                    .map_err(|error| {
                        sql_error("Failed to insert an indexed component set.", error)
                    })?;
                }
                _ => {
                    return Err(AppError::new(
                        ErrorCode::Internal,
                        "An unknown entity table was selected.",
                    ));
                }
            }
        }
        Ok(())
    }

    fn cleanup_orphan_staging(&self, minimum_age: Duration) -> AppResult<()> {
        let now = SystemTime::now();
        let entries = fs::read_dir(self.root.join("staging"))
            .map_err(|error| io_error("Failed to inspect staging files.", error))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| io_error("Failed to inspect a staging entry.", error))?;
            let metadata = entry
                .metadata()
                .map_err(|error| io_error("Failed to inspect staging metadata.", error))?;
            if !metadata.is_file() {
                continue;
            }
            let age = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok());
            if age.is_some_and(|age| age >= minimum_age) {
                fs::remove_file(entry.path())
                    .map_err(|error| io_error("Failed to remove orphan staging data.", error))?;
            }
        }
        Ok(())
    }

    async fn unreferenced_after(&self, removed_snapshots: &[String]) -> AppResult<Vec<String>> {
        if removed_snapshots.is_empty() {
            return Ok(Vec::new());
        }
        let removed = removed_snapshots
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let mut candidates = std::collections::BTreeSet::new();
        let mut retained = std::collections::BTreeSet::new();
        let mut db = self.db.clone();

        for row in Query::<toasty::stmt::List<SnapshotRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect snapshot blob references.", error))?
        {
            if removed.contains(row.id.as_str()) {
                candidates.insert(row.raw_blob_hash);
            } else {
                retained.insert(row.raw_blob_hash);
            }
        }
        for row in Query::<toasty::stmt::List<NodeRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect node blob references.", error))?
        {
            if removed.contains(row.snapshot_id.as_str()) {
                candidates.insert(row.node_blob_hash);
            } else {
                retained.insert(row.node_blob_hash);
            }
        }
        for row in Query::<toasty::stmt::List<StyleRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect style blob references.", error))?
        {
            if removed.contains(row.snapshot_id.as_str()) {
                candidates.insert(row.json_blob_hash);
            } else {
                retained.insert(row.json_blob_hash);
            }
        }
        for row in Query::<toasty::stmt::List<ComponentRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect component blob references.", error))?
        {
            if removed.contains(row.snapshot_id.as_str()) {
                candidates.insert(row.json_blob_hash);
            } else {
                retained.insert(row.json_blob_hash);
            }
        }
        for row in Query::<toasty::stmt::List<ComponentSetRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect component-set blob references.", error))?
        {
            if removed.contains(row.snapshot_id.as_str()) {
                candidates.insert(row.json_blob_hash);
            } else {
                retained.insert(row.json_blob_hash);
            }
        }
        for row in Query::<toasty::stmt::List<DerivedArtifactRow>>::all()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to inspect derived blob references.", error))?
        {
            if removed.contains(row.snapshot_id.as_str()) {
                candidates.insert(row.blob_hash);
            } else {
                retained.insert(row.blob_hash);
            }
        }
        Ok(candidates.difference(&retained).cloned().collect())
    }
}

impl SnapshotRepository for Store {
    async fn resolve_snapshot(&self, selector: SnapshotSelector<'_>) -> AppResult<SnapshotSummary> {
        let mut db = self.db.clone();
        let (snapshot_id, is_head) = if let Some(snapshot_id) = selector.snapshot_id {
            let is_head = Query::<toasty::stmt::List<HeadRow>>::all()
                .filter(HeadRow::fields().snapshot_id().eq(snapshot_id))
                .count()
                .exec(&mut db)
                .await
                .map_err(|error| sql_error("Failed to inspect snapshot heads.", error))?
                > 0;
            (snapshot_id.to_owned(), is_head)
        } else {
            let head = Query::<toasty::stmt::List<HeadRow>>::all()
                .filter(
                    HeadRow::fields().file_key().eq(selector.file_key).and(
                        HeadRow::fields()
                            .request_profile()
                            .eq(selector.request_profile),
                    ),
                )
                .first()
                .exec(&mut db)
                .await
                .map_err(|error| sql_error("Failed to resolve the snapshot head.", error))?;
            match head {
                Some(head) => (head.snapshot_id, true),
                None => return Err(snapshot_missing_error(selector)),
            }
        };
        let row = Query::<toasty::stmt::List<SnapshotRow>>::all()
            .filter(
                SnapshotRow::fields()
                    .id()
                    .eq(snapshot_id)
                    .and(SnapshotRow::fields().file_key().eq(selector.file_key))
                    .and(
                        SnapshotRow::fields()
                            .request_profile()
                            .eq(selector.request_profile),
                    ),
            )
            .first()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to resolve a snapshot.", error))?
            .ok_or_else(|| snapshot_missing_error(selector))?;
        snapshot_from_model(row, is_head)
    }

    async fn load_node(&self, snapshot_id: &str, node_id: &str) -> AppResult<StoredNode> {
        let mut db = self.db.clone();
        let row = Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(
                NodeRow::fields()
                    .snapshot_id()
                    .eq(snapshot_id)
                    .and(NodeRow::fields().node_id().eq(node_id)),
            )
            .first()
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load a node index row.", error))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::NodeNotFound,
                    "The requested node does not exist in the selected snapshot.",
                )
            })?;
        let raw_node = serde_json::from_slice(&self.blobs.get(&row.node_blob_hash)?)
            .map_err(|error| corrupt_error("A node blob does not contain valid JSON.", error))?;
        let path_ids = serde_json::from_str(&row.path_ids).map_err(|error| {
            corrupt_error("A node path index does not contain valid JSON.", error)
        })?;
        let bounds = match (row.x, row.y, row.width, row.height) {
            (Some(x), Some(y), Some(width), Some(height)) => Some(BoundingBox {
                x,
                y,
                width,
                height,
            }),
            _ => None,
        };
        let child_ids = Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(
                NodeRow::fields()
                    .snapshot_id()
                    .eq(snapshot_id)
                    .and(NodeRow::fields().parent_id().eq(Some(node_id.to_owned()))),
            )
            .order_by(NodeRow::fields().sibling_order().asc())
            .select(NodeRow::fields().node_id())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load child nodes.", error))?;
        Ok(StoredNode {
            index: IndexedNode {
                node_id: row.node_id,
                parent_id: row.parent_id,
                node_type: row.node_type,
                name: row.name,
                depth: u32::try_from(row.depth)
                    .map_err(|error| corrupt_error("A node depth cannot be represented.", error))?,
                sibling_order: u32::try_from(row.sibling_order).map_err(|error| {
                    corrupt_error("A node sibling order cannot be represented.", error)
                })?,
                path_ids,
                visible: row.visible,
                bounds,
                component_id: row.component_id,
                text_content: row.text_content,
                raw: raw_node,
                subtree_hash: row.subtree_hash,
            },
            child_ids,
        })
    }

    async fn load_diff_nodes(&self, snapshot_id: &str) -> AppResult<Vec<SnapshotDiffNode>> {
        let mut db = self.db.clone();
        let rows = Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(NodeRow::fields().snapshot_id().eq(snapshot_id))
            .order_by(NodeRow::fields().node_id().asc())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load snapshot node rows.", error))?;
        let mut nodes = Vec::with_capacity(rows.len());
        for row in rows {
            let path_ids = serde_json::from_str(&row.path_ids).map_err(|error| {
                corrupt_error("A node path index does not contain valid JSON.", error)
            })?;
            nodes.push(SnapshotDiffNode {
                node_id: row.node_id,
                parent_id: row.parent_id,
                node_type: row.node_type,
                name: row.name,
                sibling_order: u32::try_from(row.sibling_order).map_err(|error| {
                    corrupt_error("A node sibling order cannot be represented.", error)
                })?,
                path_ids,
                own_hash: row.node_blob_hash,
                subtree_hash: row.subtree_hash,
            });
        }
        Ok(nodes)
    }

    async fn root_node_id(&self, snapshot_id: &str) -> AppResult<String> {
        let mut db = self.db.clone();
        Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(NodeRow::fields().snapshot_id().eq(snapshot_id))
            .order_by(NodeRow::fields().sibling_order().asc())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to resolve the document root.", error))?
            .into_iter()
            .find(|row| row.parent_id.is_none())
            .map(|row| row.node_id)
            .ok_or_else(|| corrupt_error("A snapshot has no document root.", snapshot_id))
    }

    async fn node_candidates(
        &self,
        snapshot_id: &str,
        identifier_or_name: &str,
    ) -> AppResult<Vec<NodeSearchResult>> {
        let mut db = self.db.clone();
        let rows = toasty::sql::query(
            "SELECT node_id,name,node_type,path_ids,depth FROM nodes
             WHERE snapshot_id=?1 AND (node_id=?2 OR lower(name) LIKE '%' || lower(?2) || '%')
             ORDER BY CASE WHEN node_id=?2 THEN 0 ELSE 1 END, depth, sibling_order LIMIT 5",
        )
        .bind(snapshot_id)
        .bind(identifier_or_name)
        .exec(&mut db)
        .await
        .map_err(|error| sql_error("Failed to find node candidates.", error))?;
        decode_search_results(rows)
    }

    async fn search_nodes(
        &self,
        snapshot_id: &str,
        query: &NodeSearchQuery,
    ) -> AppResult<(Vec<NodeSearchResult>, Option<String>)> {
        let limit = if query.limit == 0 {
            100_u32
        } else {
            query.limit.min(1_000)
        };
        let offset = parse_cursor(query.cursor.as_deref())?;
        let sql_offset = i64::try_from(offset).map_err(|error| {
            AppError::new(
                ErrorCode::InvalidInput,
                "The node search cursor is too large.",
            )
            .with_details(json!({"reason": error.to_string()}))
        })?;
        let mut db = self.db.clone();
        let rows = toasty::sql::query(
            "SELECT n.node_id,n.name,n.node_type,n.path_ids,n.depth FROM nodes n
                 WHERE n.snapshot_id=?1
                   AND (?2=0 OR lower(n.name) LIKE '%' || lower(?3) || '%')
                   AND (?4=0 OR n.node_type=?5)
                   AND (?6=0 OR EXISTS(SELECT 1 FROM json_each(n.path_ids) WHERE value=?7))
                   AND (?8=0 OR EXISTS(
                     SELECT 1 FROM node_fts f
                     WHERE f.snapshot_id=n.snapshot_id AND f.node_id=n.node_id AND node_fts MATCH ?9
                   ))
                 ORDER BY n.depth,n.path_ids,n.sibling_order,n.node_id LIMIT ?10 OFFSET ?11",
        )
        .bind(snapshot_id)
        .bind(i64::from(query.name.is_some()))
        .bind(query.name.as_deref().unwrap_or_default())
        .bind(i64::from(query.node_type.is_some()))
        .bind(query.node_type.as_deref().unwrap_or_default())
        .bind(i64::from(query.ancestor_id.is_some()))
        .bind(query.ancestor_id.as_deref().unwrap_or_default())
        .bind(i64::from(query.text.is_some()))
        .bind(query.text.as_deref().unwrap_or_default())
        .bind(i64::from(limit))
        .bind(sql_offset)
        .exec(&mut db)
        .await
        .map_err(|error| sql_error("Failed to search local nodes.", error))?;
        let results = decode_search_results(rows)?;
        let next = (results.len() == usize::try_from(limit).unwrap_or(usize::MAX))
            .then(|| format!("o:{}", offset + u64::from(limit)));
        Ok((results, next))
    }

    async fn load_styles(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_styles_model(snapshot_id).await
    }

    async fn load_components(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_components_model(snapshot_id).await
    }

    async fn load_component_sets(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_component_sets_model(snapshot_id).await
    }

    async fn component_usage(
        &self,
        snapshot_id: &str,
        component_id: &str,
    ) -> AppResult<ComponentUsage> {
        let mut db = self.db.clone();
        let instance_node_ids = Query::<toasty::stmt::List<NodeRow>>::all()
            .filter(
                NodeRow::fields().snapshot_id().eq(snapshot_id).and(
                    NodeRow::fields()
                        .component_id()
                        .eq(Some(component_id.to_owned())),
                ),
            )
            .order_by(NodeRow::fields().path_ids().asc())
            .select(NodeRow::fields().node_id())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load component usage.", error))?;
        Ok(ComponentUsage {
            component_id: component_id.to_owned(),
            instance_node_ids,
        })
    }
}

impl Store {
    async fn load_styles_model(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        let mut db = self.db.clone();
        let rows = Query::<toasty::stmt::List<StyleRow>>::all()
            .filter(StyleRow::fields().snapshot_id().eq(snapshot_id))
            .order_by(StyleRow::fields().style_id().asc())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load indexed styles.", error))?;
        let mut entities = Vec::new();
        for row in rows {
            let raw =
                serde_json::from_slice(&self.blobs.get(&row.json_blob_hash)?).map_err(|error| {
                    corrupt_error("An entity blob does not contain valid JSON.", error)
                })?;
            entities.push(IndexedEntity {
                id: row.style_id,
                node_id: None,
                name: row.name,
                entity_type: row.style_type,
                raw,
            });
        }
        Ok(entities)
    }

    async fn load_components_model(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        let mut db = self.db.clone();
        let rows = Query::<toasty::stmt::List<ComponentRow>>::all()
            .filter(ComponentRow::fields().snapshot_id().eq(snapshot_id))
            .order_by(ComponentRow::fields().component_id().asc())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load indexed components.", error))?;
        self.component_entities(rows, "component")
    }

    async fn load_component_sets_model(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        let mut db = self.db.clone();
        let rows = Query::<toasty::stmt::List<ComponentSetRow>>::all()
            .filter(ComponentSetRow::fields().snapshot_id().eq(snapshot_id))
            .order_by(ComponentSetRow::fields().component_set_id().asc())
            .exec(&mut db)
            .await
            .map_err(|error| sql_error("Failed to load indexed component sets.", error))?;
        let mut entities = Vec::with_capacity(rows.len());
        for row in rows {
            entities.push(self.entity_from_blob(
                row.component_set_id,
                row.node_id,
                row.name,
                "component_set",
                &row.json_blob_hash,
            )?);
        }
        Ok(entities)
    }

    fn component_entities(
        &self,
        rows: Vec<ComponentRow>,
        entity_type: &str,
    ) -> AppResult<Vec<IndexedEntity>> {
        let mut entities = Vec::with_capacity(rows.len());
        for row in rows {
            entities.push(self.entity_from_blob(
                row.component_id,
                row.node_id,
                row.name,
                entity_type,
                &row.json_blob_hash,
            )?);
        }
        Ok(entities)
    }

    fn entity_from_blob(
        &self,
        id: String,
        node_id: Option<String>,
        name: Option<String>,
        entity_type: &str,
        hash: &str,
    ) -> AppResult<IndexedEntity> {
        let raw = serde_json::from_slice(&self.blobs.get(hash)?)
            .map_err(|error| corrupt_error("An entity blob does not contain valid JSON.", error))?;
        Ok(IndexedEntity {
            id,
            node_id,
            name,
            entity_type: entity_type.to_owned(),
            raw,
        })
    }
}

impl AttemptRecorder for Store {
    async fn record_attempt(&self, attempt: &ApiAttempt) -> AppResult<()> {
        let retry_after = attempt
            .retry_after
            .map(i64::try_from)
            .transpose()
            .map_err(|error| corrupt_error("Retry-After exceeds SQLite integer range.", error))?;
        let mut db = self.db.clone();
        toasty::create!(ApiAttemptRow {
            started_at: attempt.started_at.to_rfc3339(),
            completed_at: attempt.completed_at.to_rfc3339(),
            command: attempt.command.clone(),
            endpoint_class: endpoint_name(attempt.endpoint_class).to_owned(),
            tier: tier_name(attempt.tier).to_owned(),
            file_key: attempt.file_key.clone(),
            request_profile: attempt.request_profile.clone(),
            http_status: attempt.http_status.map(i64::from),
            error_code: attempt.error_code.clone(),
            retry_after,
            plan_tier: attempt.plan_tier.clone(),
            rate_limit_type: attempt.rate_limit_type.clone(),
        })
        .exec(&mut db)
        .await
        .map_err(|error| sql_error("Failed to record a Figma request attempt.", error))?;
        Ok(())
    }
}

fn snapshot_from_model(row: SnapshotRow, is_head: bool) -> AppResult<SnapshotSummary> {
    let fetched_at = DateTime::parse_from_rfc3339(&row.fetched_at)
        .map_err(|error| corrupt_error("A snapshot timestamp is invalid.", error))?
        .with_timezone(&Utc);
    Ok(SnapshotSummary {
        id: row.id,
        file_key: row.file_key,
        figma_version: row.figma_version,
        request_profile: row.request_profile,
        file_name: row.file_name,
        fetched_at,
        last_modified: row.last_modified,
        raw_blob_hash: row.raw_blob_hash,
        raw_size: u64::try_from(row.raw_size)
            .map_err(|error| corrupt_error("A snapshot raw size is invalid.", error))?,
        node_count: u64::try_from(row.node_count)
            .map_err(|error| corrupt_error("A snapshot node count is invalid.", error))?,
        parser_version: u32::try_from(row.parser_version)
            .map_err(|error| corrupt_error("A snapshot parser version is invalid.", error))?,
        is_head,
    })
}

fn snapshot_missing_error(selector: SnapshotSelector<'_>) -> AppError {
    let code = if selector.snapshot_id.is_some() {
        ErrorCode::SnapshotNotFound
    } else {
        ErrorCode::SnapshotMissing
    };
    AppError::new(
        code,
        "No local snapshot matches this file and request profile.",
    )
    .with_details(json!({
        "fileKey": selector.file_key,
        "requestProfile": selector.request_profile,
        "snapshotId": selector.snapshot_id,
        "suggestedCommand": format!("figstash snapshot pull {}", selector.file_key),
    }))
}

fn decode_search_results(rows: Vec<toasty::stmt::Value>) -> AppResult<Vec<NodeSearchResult>> {
    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        let toasty::stmt::Value::Record(record) = row else {
            return Err(corrupt_error(
                "A node search row has an invalid shape.",
                "expected a record",
            ));
        };
        let mut fields = record.into_iter();
        let node_id = value_string(next_value(&mut fields)?)?;
        let name = value_string(next_value(&mut fields)?)?;
        let node_type = value_string(next_value(&mut fields)?)?;
        let path_json = value_string(next_value(&mut fields)?)?;
        let depth = u32::try_from(value_i64(next_value(&mut fields)?)?)
            .map_err(|error| corrupt_error("A node search depth is invalid.", error))?;
        if fields.next().is_some() {
            return Err(corrupt_error(
                "A node search row has an invalid shape.",
                "unexpected trailing fields",
            ));
        }
        let path_ids = serde_json::from_str(&path_json)
            .map_err(|error| corrupt_error("A node path index is invalid.", error))?;
        results.push(NodeSearchResult {
            node_id,
            name,
            node_type,
            path_ids,
            depth,
        });
    }
    Ok(results)
}

fn next_value(
    fields: &mut impl Iterator<Item = toasty::stmt::Value>,
) -> AppResult<toasty::stmt::Value> {
    fields.next().ok_or_else(|| {
        corrupt_error(
            "A database row has an invalid shape.",
            "a required field is missing",
        )
    })
}

fn value_string(value: toasty::stmt::Value) -> AppResult<String> {
    match value {
        toasty::stmt::Value::String(value) => Ok(value),
        value => Err(corrupt_error(
            "A database field has an invalid type.",
            format!("expected string, got {value:?}"),
        )),
    }
}

fn value_i64(value: toasty::stmt::Value) -> AppResult<i64> {
    match value {
        toasty::stmt::Value::I64(value) => Ok(value),
        value => Err(corrupt_error(
            "A database field has an invalid type.",
            format!("expected i64, got {value:?}"),
        )),
    }
}

fn parse_cursor(cursor: Option<&str>) -> AppResult<u64> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .strip_prefix("o:")
        .and_then(|offset| offset.parse::<u64>().ok())
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidInput,
                "The node search cursor is invalid.",
            )
            .with_details(json!({"cursor": cursor}))
        })
}

fn grouped_attempt_models<'a>(
    attempts: &'a [ApiAttemptRow],
    key: impl Fn(&'a ApiAttemptRow) -> &'a str,
) -> AppResult<Vec<QuotaBucket>> {
    let mut groups = std::collections::BTreeMap::<String, (u64, u64)>::new();
    for attempt in attempts {
        let entry = groups.entry(key(attempt).to_owned()).or_default();
        entry.0 = entry
            .0
            .checked_add(1)
            .ok_or_else(|| corrupt_error("A quota grouping count overflowed.", "attempted"))?;
        if attempt
            .http_status
            .is_some_and(|status| (200..=299).contains(&status))
        {
            entry.1 = entry
                .1
                .checked_add(1)
                .ok_or_else(|| corrupt_error("A quota grouping count overflowed.", "succeeded"))?;
        }
    }
    Ok(groups
        .into_iter()
        .map(|(key, (attempted, succeeded))| QuotaBucket {
            key,
            attempted,
            succeeded,
        })
        .collect())
}

fn latest_rate_limit_model(attempts: &[ApiAttemptRow]) -> AppResult<Option<RecentRateLimit>> {
    attempts
        .iter()
        .filter(|attempt| attempt.http_status == Some(429))
        .max_by(|left, right| {
            (left.completed_at.as_str(), left.id).cmp(&(right.completed_at.as_str(), right.id))
        })
        .map(|attempt| {
            Ok(RecentRateLimit {
                completed_at: attempt.completed_at.clone(),
                retry_after: attempt.retry_after.map(u64::try_from).transpose().map_err(
                    |error| corrupt_error("A stored Retry-After value is invalid.", error),
                )?,
                plan_tier: attempt.plan_tier.clone(),
                rate_limit_type: attempt.rate_limit_type.clone(),
            })
        })
        .transpose()
}

const fn endpoint_name(endpoint: EndpointClass) -> &'static str {
    match endpoint {
        EndpointClass::GetFile => "get_file",
        EndpointClass::GetFileNodes => "get_file_nodes",
        EndpointClass::GetImages => "get_images",
        EndpointClass::GetImageFills => "get_image_fills",
        EndpointClass::GetCurrentUser => "get_current_user",
    }
}

const fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Tier1 => "tier1",
        Tier::Tier2 => "tier2",
        Tier::Tier3 => "tier3",
    }
}

pub(crate) fn restrict_directory(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_error("Failed to restrict a data directory.", error))?;
    }
    Ok(())
}

pub(crate) fn restrict_file(path: &Path) -> AppResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("Failed to restrict a data file.", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Store;
    use figstash_core::{RequestProfile, SnapshotRepository, SnapshotSelector};
    use std::fs;

    fn fixture() -> &'static [u8] {
        br#"{"name":"Example","version":"1","document":{"id":"0:0","name":"Doc","type":"DOCUMENT","children":[{"id":"1:1","name":"Title","type":"TEXT","characters":"Hello"}]},"styles":{},"components":{},"componentSets":{}}"#
    }

    #[tokio::test]
    async fn failed_precommit_does_not_move_head_and_committed_snapshot_is_queryable() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .await
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let missing = store
            .resolve_snapshot(SnapshotSelector {
                file_key: "fileKey123",
                request_profile: RequestProfile::default().key(),
                snapshot_id: None,
            })
            .await;
        assert!(missing.is_err());

        let stage = store
            .create_staging_file()
            .unwrap_or_else(|error| panic!("staging allocation failed: {error}"));
        fs::write(&stage, fixture())
            .unwrap_or_else(|error| panic!("fixture staging failed: {error}"));
        let indexed = figstash_query_for_store_test(fixture());
        let summary = store
            .commit_snapshot(
                "fileKey123",
                RequestProfile::default().key(),
                &stage,
                &indexed,
            )
            .await
            .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"));
        let node = store
            .load_node(&summary.id, "1:1")
            .await
            .unwrap_or_else(|error| panic!("node lookup failed: {error}"));
        assert_eq!(node.index.text_content.as_deref(), Some("Hello"));

        let failed_stage = store
            .create_staging_file()
            .unwrap_or_else(|error| panic!("failed staging allocation failed: {error}"));
        fs::write(&failed_stage, fixture())
            .unwrap_or_else(|error| panic!("failed fixture staging failed: {error}"));
        let mut invalid_index = figstash_query_for_store_test(fixture());
        invalid_index.figma_version = "2".to_owned();
        invalid_index.nodes.push(invalid_index.nodes[1].clone());
        assert!(
            store
                .commit_snapshot(
                    "fileKey123",
                    RequestProfile::default().key(),
                    &failed_stage,
                    &invalid_index,
                )
                .await
                .is_err()
        );
        let unchanged = store
            .resolve_snapshot(SnapshotSelector {
                file_key: "fileKey123",
                request_profile: RequestProfile::default().key(),
                snapshot_id: None,
            })
            .await
            .unwrap_or_else(|error| panic!("head lookup failed: {error}"));
        assert_eq!(unchanged.id, summary.id);

        drop(store);
        let reopened = Store::open(temporary.path())
            .await
            .unwrap_or_else(|error| panic!("existing catalog failed to reopen: {error}"));
        let persisted = reopened
            .resolve_snapshot(SnapshotSelector {
                file_key: "fileKey123",
                request_profile: RequestProfile::default().key(),
                snapshot_id: None,
            })
            .await
            .unwrap_or_else(|error| panic!("reopened head lookup failed: {error}"));
        assert_eq!(persisted.id, summary.id);
        let persisted_node = reopened
            .load_node(&persisted.id, "1:1")
            .await
            .unwrap_or_else(|error| panic!("reopened node lookup failed: {error}"));
        assert_eq!(persisted_node.index.text_content.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn writer_lock_is_exclusive_and_store_permissions_are_private() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .await
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let _first = store
            .try_lock("fileKey123", RequestProfile::default().key())
            .unwrap_or_else(|error| panic!("first writer lock failed: {error}"));
        assert!(
            store
                .try_lock("fileKey123", RequestProfile::default().key())
                .is_err()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let directory_mode = fs::metadata(store.root())
                .unwrap_or_else(|error| panic!("store metadata failed: {error}"))
                .permissions()
                .mode()
                & 0o777;
            let catalog_mode = fs::metadata(store.root().join("catalog.sqlite3"))
                .unwrap_or_else(|error| panic!("catalog metadata failed: {error}"))
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(directory_mode, 0o700);
            assert_eq!(catalog_mode, 0o600);
        }
    }

    #[tokio::test]
    async fn prune_requires_execution_and_never_deletes_the_current_or_only_snapshot() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .await
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let first_stage = store
            .create_staging_file()
            .unwrap_or_else(|error| panic!("first staging allocation failed: {error}"));
        fs::write(&first_stage, fixture())
            .unwrap_or_else(|error| panic!("first fixture staging failed: {error}"));
        let first_index = figstash_query_for_store_test(fixture());
        let first = store
            .commit_snapshot(
                "fileKey123",
                RequestProfile::default().key(),
                &first_stage,
                &first_index,
            )
            .await
            .unwrap_or_else(|error| panic!("first snapshot commit failed: {error}"));
        assert!(
            store
                .prune_plan()
                .await
                .unwrap_or_else(|error| panic!("first prune plan failed: {error}"))
                .snapshot_ids
                .is_empty()
        );

        let second_stage = store
            .create_staging_file()
            .unwrap_or_else(|error| panic!("second staging allocation failed: {error}"));
        let mut second_bytes = fixture().to_vec();
        second_bytes.push(b'\n');
        fs::write(&second_stage, &second_bytes)
            .unwrap_or_else(|error| panic!("second fixture staging failed: {error}"));
        let mut second_index = figstash_query_for_store_test(fixture());
        second_index.figma_version = "2".to_owned();
        let second = store
            .commit_snapshot(
                "fileKey123",
                RequestProfile::default().key(),
                &second_stage,
                &second_index,
            )
            .await
            .unwrap_or_else(|error| panic!("second snapshot commit failed: {error}"));
        let plan = store
            .prune_plan()
            .await
            .unwrap_or_else(|error| panic!("prune plan failed: {error}"));
        assert_eq!(plan.snapshot_ids, [first.id]);
        assert!(!plan.executed);
        assert_eq!(
            store
                .list_snapshots(None)
                .await
                .unwrap_or_else(|error| panic!("pre-prune list failed: {error}"))
                .len(),
            2
        );

        let executed = store
            .execute_prune()
            .await
            .unwrap_or_else(|error| panic!("prune execution failed: {error}"));
        assert!(executed.executed);
        let remaining = store
            .list_snapshots(None)
            .await
            .unwrap_or_else(|error| panic!("post-prune list failed: {error}"));
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second.id);
        assert!(remaining[0].is_head);
    }

    fn figstash_query_for_store_test(bytes: &[u8]) -> figstash_core::IndexedSnapshot {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .unwrap_or_else(|error| panic!("fixture parse failed: {error}"));
        let document = value["document"].clone();
        let child = document["children"][0].clone();
        figstash_core::IndexedSnapshot {
            file_name: "Example".to_owned(),
            figma_version: "1".to_owned(),
            last_modified: None,
            nodes: vec![
                test_node(document, "0:0", None, 0, 0, vec!["0:0"], None),
                test_node(
                    child,
                    "1:1",
                    Some("0:0"),
                    1,
                    0,
                    vec!["0:0", "1:1"],
                    Some("Hello"),
                ),
            ],
            styles: Vec::new(),
            components: Vec::new(),
            component_sets: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn test_node(
        mut raw: serde_json::Value,
        id: &str,
        parent: Option<&str>,
        depth: u32,
        order: u32,
        path: Vec<&str>,
        text: Option<&str>,
    ) -> figstash_core::IndexedNode {
        if let Some(object) = raw.as_object_mut() {
            object.remove("children");
        }
        figstash_core::IndexedNode {
            node_id: id.to_owned(),
            parent_id: parent.map(ToOwned::to_owned),
            node_type: if depth == 0 { "DOCUMENT" } else { "TEXT" }.to_owned(),
            name: if depth == 0 { "Doc" } else { "Title" }.to_owned(),
            depth,
            sibling_order: order,
            path_ids: path.into_iter().map(ToOwned::to_owned).collect(),
            visible: None,
            bounds: None,
            component_id: None,
            text_content: text.map(ToOwned::to_owned),
            raw,
            subtree_hash: blake3::hash(id.as_bytes()).to_hex().to_string(),
        }
    }
}
