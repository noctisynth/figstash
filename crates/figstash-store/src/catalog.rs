//! `SQLite` catalog and atomic snapshot transactions.

use crate::blob::BlobStore;
use crate::error::{corrupt_error, io_error, sql_error};
use chrono::{DateTime, Datelike, Utc};
use figstash_core::{
    ApiAttempt, AppError, AppResult, AttemptRecorder, BoundingBox, ComponentUsage, EndpointClass,
    ErrorCode, IndexedEntity, IndexedNode, IndexedSnapshot, NodeSearchQuery, NodeSearchResult,
    PARSER_VERSION, SnapshotRepository, SnapshotSelector, SnapshotSummary, StoredNode, Tier,
};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
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
    pub fn open(data_directory: impl AsRef<Path>) -> AppResult<Self> {
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
        let store = Self {
            blobs: BlobStore::new(root.join("blobs")),
            root,
            catalog_path,
        };
        store.initialize_catalog()?;
        store.cleanup_orphan_staging(Duration::from_secs(24 * 60 * 60))?;
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
    pub fn commit_snapshot(
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

        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| sql_error("Failed to start the snapshot transaction.", error))?;
        self.insert_snapshot(&transaction, &summary, indexed)?;
        transaction
            .execute(
                "INSERT INTO heads(file_key, request_profile, snapshot_id) VALUES(?1, ?2, ?3)
                 ON CONFLICT(file_key, request_profile) DO UPDATE SET snapshot_id=excluded.snapshot_id",
                params![file_key, request_profile, summary.id],
            )
            .map_err(|error| sql_error("Failed to advance the snapshot HEAD.", error))?;
        transaction
            .commit()
            .map_err(|error| sql_error("Failed to commit the snapshot transaction.", error))?;
        let _ = fs::remove_file(staged_response);
        self.resolve_snapshot(SnapshotSelector {
            file_key,
            request_profile,
            snapshot_id: Some(&summary.id),
        })
    }

    /// Lists immutable snapshots newest first.
    ///
    /// # Errors
    ///
    /// Returns a storage or integrity error when catalog rows cannot be loaded.
    pub fn list_snapshots(&self, file_key: Option<&str>) -> AppResult<Vec<SnapshotSummary>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT s.id,s.file_key,s.figma_version,s.request_profile,s.file_name,
                        s.fetched_at,s.last_modified,s.raw_blob_hash,s.raw_size,s.node_count,
                        s.parser_version,
                        EXISTS(SELECT 1 FROM heads h WHERE h.snapshot_id=s.id)
                 FROM snapshots s
                 WHERE (?1 IS NULL OR s.file_key=?1)
                 ORDER BY s.fetched_at DESC, s.id",
            )
            .map_err(|error| sql_error("Failed to prepare snapshot listing.", error))?;
        let rows = statement
            .query_map(params![file_key], snapshot_from_row)
            .map_err(|error| sql_error("Failed to list snapshots.", error))?;
        collect_rows(rows, "Failed to decode snapshot metadata.")
    }

    /// Creates a safe plan retaining every current HEAD and every lineage's sole snapshot.
    ///
    /// # Errors
    ///
    /// Returns a storage error when reachability cannot be calculated.
    pub fn prune_plan(&self) -> AppResult<PrunePlan> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT s.id FROM snapshots s
                 WHERE NOT EXISTS(SELECT 1 FROM heads h WHERE h.snapshot_id=s.id)
                   AND EXISTS(
                     SELECT 1 FROM snapshots newer
                     WHERE newer.file_key=s.file_key AND newer.request_profile=s.request_profile
                       AND newer.id<>s.id
                   )
                 ORDER BY s.fetched_at, s.id",
            )
            .map_err(|error| sql_error("Failed to prepare the prune plan.", error))?;
        let snapshot_ids = collect_rows(
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| sql_error("Failed to build the prune plan.", error))?,
            "Failed to decode a prune candidate.",
        )?;
        let blob_hashes = Self::unreferenced_after(&connection, &snapshot_ids)?;
        let retained_snapshots = connection
            .query_row("SELECT COUNT(*) FROM snapshots", [], |row| {
                nonnegative_integer(row, 0)
            })
            .map_err(|error| sql_error("Failed to count retained snapshots.", error))?
            .saturating_sub(u64::try_from(snapshot_ids.len()).unwrap_or(u64::MAX));
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
    pub fn execute_prune(&self) -> AppResult<PrunePlan> {
        let mut plan = self.prune_plan()?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| sql_error("Failed to start the prune transaction.", error))?;
        for snapshot_id in &plan.snapshot_ids {
            transaction
                .execute("DELETE FROM node_fts WHERE snapshot_id=?1", [snapshot_id])
                .map_err(|error| sql_error("Failed to prune node search rows.", error))?;
            transaction
                .execute("DELETE FROM snapshots WHERE id=?1", [snapshot_id])
                .map_err(|error| sql_error("Failed to prune a snapshot.", error))?;
        }
        transaction
            .commit()
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
    pub fn quota_status(&self) -> AppResult<QuotaStatus> {
        let connection = self.connection()?;
        let now = Utc::now();
        let month = format!("{:04}-{:02}", now.year(), now.month());
        let prefix = format!("{month}%");
        let (attempted, succeeded) = connection
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN http_status BETWEEN 200 AND 299 THEN 1 ELSE 0 END),0)
                 FROM api_attempts WHERE completed_at LIKE ?1",
                [&prefix],
                |row| Ok((nonnegative_integer(row, 0)?, nonnegative_integer(row, 1)?)),
            )
            .map_err(|error| sql_error("Failed to summarize the request ledger.", error))?;
        Ok(QuotaStatus {
            scope: "figstash_observed_only".to_owned(),
            month,
            attempted,
            succeeded,
            by_tier: grouped_attempts(&connection, "tier", &prefix)?,
            by_endpoint: grouped_attempts(&connection, "endpoint_class", &prefix)?,
            by_command: grouped_attempts(&connection, "command", &prefix)?,
            by_file: grouped_attempts(&connection, "COALESCE(file_key, 'none')", &prefix)?,
            recent_rate_limit: latest_rate_limit(&connection)?,
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

    fn initialize_catalog(&self) -> AppResult<()> {
        let connection = self.connection()?;
        connection
            .execute_batch(MIGRATION_SQL)
            .map_err(|error| sql_error("Failed to initialize the snapshot catalog.", error))?;
        let checksum = blake3::hash(MIGRATION_SQL.as_bytes()).to_hex().to_string();
        let existing = connection
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version=?1",
                [MIGRATION_VERSION],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| sql_error("Failed to inspect catalog migrations.", error))?;
        if let Some(existing) = existing {
            if existing != checksum {
                return Err(corrupt_error(
                    "The catalog migration checksum does not match this binary.",
                    format!("version {MIGRATION_VERSION}"),
                ));
            }
        } else {
            connection
                .execute(
                    "INSERT INTO schema_migrations(version,applied_at,checksum) VALUES(?1,?2,?3)",
                    params![MIGRATION_VERSION, Utc::now().to_rfc3339(), checksum],
                )
                .map_err(|error| sql_error("Failed to record the catalog migration.", error))?;
        }
        restrict_file(&self.catalog_path)
    }

    fn connection(&self) -> AppResult<Connection> {
        let connection = Connection::open(&self.catalog_path)
            .map_err(|error| sql_error("Failed to open the snapshot catalog.", error))?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;",
            )
            .map_err(|error| sql_error("Failed to configure the snapshot catalog.", error))?;
        Ok(connection)
    }

    fn insert_snapshot(
        &self,
        transaction: &Transaction<'_>,
        summary: &SnapshotSummary,
        indexed: &IndexedSnapshot,
    ) -> AppResult<()> {
        let raw_size = i64::try_from(summary.raw_size).map_err(|error| {
            corrupt_error("The raw snapshot size exceeds SQLite integer range.", error)
        })?;
        let node_count = i64::try_from(summary.node_count).map_err(|error| {
            corrupt_error("The node count exceeds SQLite integer range.", error)
        })?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO snapshots(
                  id,file_key,figma_version,request_profile,file_name,fetched_at,last_modified,
                  raw_blob_hash,raw_size,node_count,parser_version,status
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'ready')",
                params![
                    summary.id,
                    summary.file_key,
                    summary.figma_version,
                    summary.request_profile,
                    summary.file_name,
                    summary.fetched_at.to_rfc3339(),
                    summary.last_modified,
                    summary.raw_blob_hash,
                    raw_size,
                    node_count,
                    summary.parser_version,
                ],
            )
            .map_err(|error| sql_error("Failed to insert snapshot metadata.", error))?;
        let already_indexed: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE snapshot_id=?1)",
                [&summary.id],
                |row| row.get(0),
            )
            .map_err(|error| sql_error("Failed to inspect existing snapshot indexes.", error))?;
        if already_indexed {
            return Ok(());
        }
        for node in &indexed.nodes {
            self.insert_node(transaction, &summary.id, node)?;
        }
        self.insert_entities(transaction, "styles", &summary.id, &indexed.styles)?;
        self.insert_entities(transaction, "components", &summary.id, &indexed.components)?;
        self.insert_entities(
            transaction,
            "component_sets",
            &summary.id,
            &indexed.component_sets,
        )?;
        Ok(())
    }

    fn insert_node(
        &self,
        transaction: &Transaction<'_>,
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
        transaction
            .execute(
                "INSERT INTO nodes(
                  snapshot_id,node_id,parent_id,node_type,name,depth,sibling_order,path_ids,
                  visible,x,y,width,height,component_id,text_content,node_blob_hash,subtree_hash
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    snapshot_id,
                    node.node_id,
                    node.parent_id,
                    node.node_type,
                    node.name,
                    node.depth,
                    node.sibling_order,
                    path_ids,
                    node.visible,
                    has_bounds.then_some(bounds.x),
                    has_bounds.then_some(bounds.y),
                    has_bounds.then_some(bounds.width),
                    has_bounds.then_some(bounds.height),
                    node.component_id,
                    node.text_content,
                    blob_hash,
                    node.subtree_hash,
                ],
            )
            .map_err(|error| sql_error("Failed to insert a node index row.", error))?;
        transaction
            .execute(
                "INSERT INTO node_fts(snapshot_id,node_id,name,text_content) VALUES(?1,?2,?3,?4)",
                params![snapshot_id, node.node_id, node.name, node.text_content],
            )
            .map_err(|error| sql_error("Failed to insert a node search row.", error))?;
        Ok(())
    }

    fn insert_entities(
        &self,
        transaction: &Transaction<'_>,
        table: &str,
        snapshot_id: &str,
        entities: &[IndexedEntity],
    ) -> AppResult<()> {
        let (id_column, type_column) = match table {
            "styles" => ("style_id", Some("style_type")),
            "components" => ("component_id", None),
            "component_sets" => ("component_set_id", None),
            _ => {
                return Err(AppError::new(
                    ErrorCode::Internal,
                    "An unknown entity table was selected.",
                ));
            }
        };
        for entity in entities {
            let raw = serde_json::to_vec(&entity.raw)
                .map_err(|error| corrupt_error("Failed to serialize an indexed entity.", error))?;
            let blob_hash = self.blobs.put_bytes(&raw)?;
            let sql = if let Some(type_column) = type_column {
                format!(
                    "INSERT INTO {table}(snapshot_id,{id_column},{type_column},name,json_blob_hash) VALUES(?1,?2,?3,?4,?5)"
                )
            } else {
                format!(
                    "INSERT INTO {table}(snapshot_id,{id_column},node_id,name,json_blob_hash) VALUES(?1,?2,?3,?4,?5)"
                )
            };
            transaction
                .execute(
                    &sql,
                    params![
                        snapshot_id,
                        entity.id,
                        if type_column.is_some() {
                            Some(entity.entity_type.as_str())
                        } else {
                            entity.node_id.as_deref()
                        },
                        entity.name,
                        blob_hash,
                    ],
                )
                .map_err(|error| sql_error("Failed to insert an indexed entity.", error))?;
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

    fn unreferenced_after(
        connection: &Connection,
        removed_snapshots: &[String],
    ) -> AppResult<Vec<String>> {
        if removed_snapshots.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = connection
            .prepare(
                "SELECT raw_blob_hash FROM snapshots
                 UNION SELECT node_blob_hash FROM nodes
                 UNION SELECT json_blob_hash FROM styles
                 UNION SELECT json_blob_hash FROM components
                 UNION SELECT json_blob_hash FROM component_sets
                 UNION SELECT blob_hash FROM derived_artifacts",
            )
            .map_err(|error| sql_error("Failed to inspect blob references.", error))?;
        let all_hashes: Vec<String> = collect_rows(
            statement
                .query_map([], |row| row.get(0))
                .map_err(|error| sql_error("Failed to inspect blob references.", error))?,
            "Failed to decode a blob reference.",
        )?;
        let mut candidates = Vec::new();
        for hash in all_hashes {
            let referenced_by_retained: bool = connection
                .query_row(
                    "SELECT
                       EXISTS(SELECT 1 FROM snapshots WHERE raw_blob_hash=?1 AND id NOT IN (SELECT value FROM json_each(?2)))
                       OR EXISTS(SELECT 1 FROM nodes WHERE node_blob_hash=?1 AND snapshot_id NOT IN (SELECT value FROM json_each(?2)))
                       OR EXISTS(SELECT 1 FROM styles WHERE json_blob_hash=?1 AND snapshot_id NOT IN (SELECT value FROM json_each(?2)))
                       OR EXISTS(SELECT 1 FROM components WHERE json_blob_hash=?1 AND snapshot_id NOT IN (SELECT value FROM json_each(?2)))
                       OR EXISTS(SELECT 1 FROM component_sets WHERE json_blob_hash=?1 AND snapshot_id NOT IN (SELECT value FROM json_each(?2)))
                       OR EXISTS(SELECT 1 FROM derived_artifacts WHERE blob_hash=?1 AND snapshot_id NOT IN (SELECT value FROM json_each(?2)))",
                    params![hash, serde_json::to_string(removed_snapshots).map_err(|error| corrupt_error("Failed to serialize prune candidates.", error))?],
                    |row| row.get(0),
                )
                .map_err(|error| sql_error("Failed to calculate blob reachability.", error))?;
            if !referenced_by_retained {
                candidates.push(hash);
            }
        }
        candidates.sort();
        candidates.dedup();
        Ok(candidates)
    }
}

impl SnapshotRepository for Store {
    fn resolve_snapshot(&self, selector: SnapshotSelector<'_>) -> AppResult<SnapshotSummary> {
        let connection = self.connection()?;
        let sql = if selector.snapshot_id.is_some() {
            "SELECT s.id,s.file_key,s.figma_version,s.request_profile,s.file_name,
                    s.fetched_at,s.last_modified,s.raw_blob_hash,s.raw_size,s.node_count,
                    s.parser_version,EXISTS(SELECT 1 FROM heads h WHERE h.snapshot_id=s.id)
             FROM snapshots s WHERE s.id=?3 AND s.file_key=?1 AND s.request_profile=?2"
        } else {
            "SELECT s.id,s.file_key,s.figma_version,s.request_profile,s.file_name,
                    s.fetched_at,s.last_modified,s.raw_blob_hash,s.raw_size,s.node_count,
                    s.parser_version,1
             FROM heads h JOIN snapshots s ON s.id=h.snapshot_id
             WHERE h.file_key=?1 AND h.request_profile=?2 AND ?3 IS NULL"
        };
        connection
            .query_row(
                sql,
                params![
                    selector.file_key,
                    selector.request_profile,
                    selector.snapshot_id
                ],
                snapshot_from_row,
            )
            .optional()
            .map_err(|error| sql_error("Failed to resolve a snapshot.", error))?
            .ok_or_else(|| {
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
            })
    }

    fn load_node(&self, snapshot_id: &str, node_id: &str) -> AppResult<StoredNode> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT node_id,parent_id,node_type,name,depth,sibling_order,path_ids,visible,
                        x,y,width,height,component_id,text_content,node_blob_hash,subtree_hash
                 FROM nodes WHERE snapshot_id=?1 AND node_id=?2",
                params![snapshot_id, node_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, u32>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<bool>>(7)?,
                        row.get::<_, Option<f64>>(8)?,
                        row.get::<_, Option<f64>>(9)?,
                        row.get::<_, Option<f64>>(10)?,
                        row.get::<_, Option<f64>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, String>(15)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| sql_error("Failed to load a node index row.", error))?
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::NodeNotFound,
                    "The requested node does not exist in the selected snapshot.",
                )
            })?;
        let raw_node = serde_json::from_slice(&self.blobs.get(&row.14)?)
            .map_err(|error| corrupt_error("A node blob does not contain valid JSON.", error))?;
        let path_ids = serde_json::from_str(&row.6).map_err(|error| {
            corrupt_error("A node path index does not contain valid JSON.", error)
        })?;
        let bounds = match (row.8, row.9, row.10, row.11) {
            (Some(x), Some(y), Some(width), Some(height)) => Some(BoundingBox {
                x,
                y,
                width,
                height,
            }),
            _ => None,
        };
        let mut child_statement = connection
            .prepare(
                "SELECT node_id FROM nodes WHERE snapshot_id=?1 AND parent_id=?2 ORDER BY sibling_order",
            )
            .map_err(|error| sql_error("Failed to prepare child node lookup.", error))?;
        let child_ids = collect_rows(
            child_statement
                .query_map(params![snapshot_id, node_id], |child| child.get(0))
                .map_err(|error| sql_error("Failed to load child nodes.", error))?,
            "Failed to decode a child node identifier.",
        )?;
        Ok(StoredNode {
            index: IndexedNode {
                node_id: row.0,
                parent_id: row.1,
                node_type: row.2,
                name: row.3,
                depth: row.4,
                sibling_order: row.5,
                path_ids,
                visible: row.7,
                bounds,
                component_id: row.12,
                text_content: row.13,
                raw: raw_node,
                subtree_hash: row.15,
            },
            child_ids,
        })
    }

    fn root_node_id(&self, snapshot_id: &str) -> AppResult<String> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT node_id FROM nodes WHERE snapshot_id=?1 AND parent_id IS NULL ORDER BY sibling_order LIMIT 1",
                [snapshot_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| sql_error("Failed to resolve the document root.", error))?
            .ok_or_else(|| corrupt_error("A snapshot has no document root.", snapshot_id))
    }

    fn node_candidates(
        &self,
        snapshot_id: &str,
        identifier_or_name: &str,
    ) -> AppResult<Vec<NodeSearchResult>> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT node_id,name,node_type,path_ids,depth FROM nodes
                 WHERE snapshot_id=?1 AND (node_id=?2 OR lower(name) LIKE '%' || lower(?2) || '%')
                 ORDER BY CASE WHEN node_id=?2 THEN 0 ELSE 1 END, depth, sibling_order LIMIT 5",
            )
            .map_err(|error| sql_error("Failed to prepare node candidates.", error))?;
        let rows = statement
            .query_map(
                params![snapshot_id, identifier_or_name],
                search_result_from_row,
            )
            .map_err(|error| sql_error("Failed to find node candidates.", error))?;
        collect_rows(rows, "Failed to decode a node candidate.")
    }

    fn search_nodes(
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
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT n.node_id,n.name,n.node_type,n.path_ids,n.depth FROM nodes n
                 WHERE n.snapshot_id=?1
                   AND (?2 IS NULL OR lower(n.name) LIKE '%' || lower(?2) || '%')
                   AND (?3 IS NULL OR n.node_type=?3)
                   AND (?4 IS NULL OR EXISTS(SELECT 1 FROM json_each(n.path_ids) WHERE value=?4))
                   AND (?5 IS NULL OR EXISTS(
                     SELECT 1 FROM node_fts f
                     WHERE f.snapshot_id=n.snapshot_id AND f.node_id=n.node_id AND node_fts MATCH ?5
                   ))
                 ORDER BY n.depth,n.path_ids,n.sibling_order,n.node_id LIMIT ?6 OFFSET ?7",
            )
            .map_err(|error| sql_error("Failed to prepare local node search.", error))?;
        let rows = statement
            .query_map(
                params![
                    snapshot_id,
                    query.name,
                    query.node_type,
                    query.ancestor_id,
                    query.text,
                    limit,
                    sql_offset,
                ],
                search_result_from_row,
            )
            .map_err(|error| sql_error("Failed to search local nodes.", error))?;
        let results = collect_rows(rows, "Failed to decode a node search result.")?;
        let next = (results.len() == usize::try_from(limit).unwrap_or(usize::MAX))
            .then(|| format!("o:{}", offset + u64::from(limit)));
        Ok((results, next))
    }

    fn load_styles(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_entities("styles", "style_id", snapshot_id, true)
    }

    fn load_components(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_entities("components", "component_id", snapshot_id, false)
    }

    fn load_component_sets(&self, snapshot_id: &str) -> AppResult<Vec<IndexedEntity>> {
        self.load_entities("component_sets", "component_set_id", snapshot_id, false)
    }

    fn component_usage(&self, snapshot_id: &str, component_id: &str) -> AppResult<ComponentUsage> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT node_id FROM nodes WHERE snapshot_id=?1 AND component_id=?2 ORDER BY path_ids",
            )
            .map_err(|error| sql_error("Failed to prepare component usage lookup.", error))?;
        let instance_node_ids = collect_rows(
            statement
                .query_map(params![snapshot_id, component_id], |row| row.get(0))
                .map_err(|error| sql_error("Failed to load component usage.", error))?,
            "Failed to decode component usage.",
        )?;
        Ok(ComponentUsage {
            component_id: component_id.to_owned(),
            instance_node_ids,
        })
    }
}

impl Store {
    fn load_entities(
        &self,
        table: &str,
        id_column: &str,
        snapshot_id: &str,
        styles: bool,
    ) -> AppResult<Vec<IndexedEntity>> {
        if !matches!(table, "styles" | "components" | "component_sets") {
            return Err(AppError::new(
                ErrorCode::Internal,
                "An unknown entity table was selected.",
            ));
        }
        let connection = self.connection()?;
        let third_column = if styles { "style_type" } else { "node_id" };
        let sql = format!(
            "SELECT {id_column},{third_column},name,json_blob_hash FROM {table} WHERE snapshot_id=?1 ORDER BY {id_column}"
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(|error| sql_error("Failed to prepare entity lookup.", error))?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| sql_error("Failed to load indexed entities.", error))?;
        let mut entities = Vec::new();
        for row in rows {
            let (id, third, name, hash) =
                row.map_err(|error| sql_error("Failed to decode an indexed entity.", error))?;
            let raw = serde_json::from_slice(&self.blobs.get(&hash)?).map_err(|error| {
                corrupt_error("An entity blob does not contain valid JSON.", error)
            })?;
            entities.push(IndexedEntity {
                id,
                node_id: if styles { None } else { third.clone() },
                name,
                entity_type: if styles {
                    third.unwrap_or_else(|| "style".to_owned())
                } else if table == "components" {
                    "component".to_owned()
                } else {
                    "component_set".to_owned()
                },
                raw,
            });
        }
        Ok(entities)
    }
}

impl AttemptRecorder for Store {
    fn record_attempt(&self, attempt: &ApiAttempt) -> AppResult<()> {
        let connection = self.connection()?;
        let retry_after = attempt
            .retry_after
            .map(i64::try_from)
            .transpose()
            .map_err(|error| corrupt_error("Retry-After exceeds SQLite integer range.", error))?;
        connection
            .execute(
                "INSERT INTO api_attempts(
                  started_at,completed_at,command,endpoint_class,tier,file_key,request_profile,
                  http_status,error_code,retry_after,plan_tier,rate_limit_type
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    attempt.started_at.to_rfc3339(),
                    attempt.completed_at.to_rfc3339(),
                    attempt.command,
                    endpoint_name(attempt.endpoint_class),
                    tier_name(attempt.tier),
                    attempt.file_key,
                    attempt.request_profile,
                    attempt.http_status,
                    attempt.error_code,
                    retry_after,
                    attempt.plan_tier,
                    attempt.rate_limit_type,
                ],
            )
            .map_err(|error| sql_error("Failed to record a Figma request attempt.", error))?;
        Ok(())
    }
}

fn snapshot_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SnapshotSummary> {
    let fetched: String = row.get(5)?;
    let fetched_at = DateTime::parse_from_rfc3339(&fetched)
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?
        .with_timezone(&Utc);
    Ok(SnapshotSummary {
        id: row.get(0)?,
        file_key: row.get(1)?,
        figma_version: row.get(2)?,
        request_profile: row.get(3)?,
        file_name: row.get(4)?,
        fetched_at,
        last_modified: row.get(6)?,
        raw_blob_hash: row.get(7)?,
        raw_size: nonnegative_integer(row, 8)?,
        node_count: nonnegative_integer(row, 9)?,
        parser_version: row.get(10)?,
        is_head: row.get(11)?,
    })
}

fn search_result_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeSearchResult> {
    let path_json: String = row.get(3)?;
    let path_ids = serde_json::from_str(&path_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(NodeSearchResult {
        node_id: row.get(0)?,
        name: row.get(1)?,
        node_type: row.get(2)?,
        path_ids,
        depth: row.get(4)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    context: &'static str,
) -> AppResult<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| sql_error(context, error))
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

fn grouped_attempts(
    connection: &Connection,
    column: &str,
    month_prefix: &str,
) -> AppResult<Vec<QuotaBucket>> {
    if !matches!(
        column,
        "tier" | "endpoint_class" | "command" | "COALESCE(file_key, 'none')"
    ) {
        return Err(AppError::new(
            ErrorCode::Internal,
            "An unknown quota grouping was selected.",
        ));
    }
    let sql = format!(
        "SELECT {column},COUNT(*),COALESCE(SUM(CASE WHEN http_status BETWEEN 200 AND 299 THEN 1 ELSE 0 END),0)
         FROM api_attempts WHERE completed_at LIKE ?1 GROUP BY {column} ORDER BY {column}"
    );
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| sql_error("Failed to prepare a quota grouping.", error))?;
    let rows = statement
        .query_map([month_prefix], |row| {
            Ok(QuotaBucket {
                key: row.get(0)?,
                attempted: nonnegative_integer(row, 1)?,
                succeeded: nonnegative_integer(row, 2)?,
            })
        })
        .map_err(|error| sql_error("Failed to summarize quota grouping.", error))?;
    collect_rows(rows, "Failed to decode a quota grouping.")
}

fn latest_rate_limit(connection: &Connection) -> AppResult<Option<RecentRateLimit>> {
    connection
        .query_row(
            "SELECT completed_at,retry_after,plan_tier,rate_limit_type FROM api_attempts
             WHERE http_status=429 ORDER BY completed_at DESC,id DESC LIMIT 1",
            [],
            |row| {
                Ok(RecentRateLimit {
                    completed_at: row.get(0)?,
                    retry_after: optional_nonnegative_integer(row, 1)?,
                    plan_tier: row.get(2)?,
                    rate_limit_type: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| sql_error("Failed to load the latest rate limit response.", error))
}

const fn endpoint_name(endpoint: EndpointClass) -> &'static str {
    match endpoint {
        EndpointClass::GetFile => "get_file",
        EndpointClass::GetFileNodes => "get_file_nodes",
        EndpointClass::GetImages => "get_images",
        EndpointClass::GetImageFills => "get_image_fills",
        EndpointClass::GetFileMeta => "get_file_meta",
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

fn nonnegative_integer(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn optional_nonnegative_integer(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(u64::try_from)
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })
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

    #[test]
    fn failed_precommit_does_not_move_head_and_committed_snapshot_is_queryable() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
            .unwrap_or_else(|error| panic!("store initialization failed: {error}"));
        let missing = store.resolve_snapshot(SnapshotSelector {
            file_key: "fileKey123",
            request_profile: RequestProfile::default().key(),
            snapshot_id: None,
        });
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
            .unwrap_or_else(|error| panic!("snapshot commit failed: {error}"));
        let node = store
            .load_node(&summary.id, "1:1")
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
                .is_err()
        );
        let unchanged = store
            .resolve_snapshot(SnapshotSelector {
                file_key: "fileKey123",
                request_profile: RequestProfile::default().key(),
                snapshot_id: None,
            })
            .unwrap_or_else(|error| panic!("head lookup failed: {error}"));
        assert_eq!(unchanged.id, summary.id);
    }

    #[test]
    fn writer_lock_is_exclusive_and_store_permissions_are_private() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
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

    #[test]
    fn prune_requires_execution_and_never_deletes_the_current_or_only_snapshot() {
        let temporary = tempfile::tempdir()
            .unwrap_or_else(|error| panic!("temporary directory failed: {error}"));
        let store = Store::open(temporary.path())
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
            .unwrap_or_else(|error| panic!("first snapshot commit failed: {error}"));
        assert!(
            store
                .prune_plan()
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
            .unwrap_or_else(|error| panic!("second snapshot commit failed: {error}"));
        let plan = store
            .prune_plan()
            .unwrap_or_else(|error| panic!("prune plan failed: {error}"));
        assert_eq!(plan.snapshot_ids, [first.id]);
        assert!(!plan.executed);
        assert_eq!(
            store
                .list_snapshots(None)
                .unwrap_or_else(|error| panic!("pre-prune list failed: {error}"))
                .len(),
            2
        );

        let executed = store
            .execute_prune()
            .unwrap_or_else(|error| panic!("prune execution failed: {error}"));
        assert!(executed.executed);
        let remaining = store
            .list_snapshots(None)
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
