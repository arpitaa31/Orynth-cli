use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use rusqlite::{Connection, OptionalExtension, Params, TransactionBehavior, params};

use super::durable::{decode_event, encode_event};
use super::{
    AgentState, AgentStatus, ArtifactError, ArtifactRef, ArtifactStore, BranchId, BranchMetadata,
    BranchStore, ContentHash, ContentHasher, DeterministicContentHasher, Event, EventKind,
    EventStore, InMemoryEventStore, RecordedReplay, ReplayMode, RunId, RunStatus, RuntimeSnapshot,
    RuntimeState, Sequence, SnapshotStore, StoreError, StoredArtifact, StoredEvent, TaskState,
    TaskStatus, validate_snapshot,
};
use orynth_kernel::{AgentId, AgentIdentity, ModelClass, ModelRef, TaskId, TrustOrigin, Usage};

const SCHEMA_VERSION: i64 = 2;
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    sequence INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    run_id TEXT NOT NULL,
    occurred_at_ms TEXT NOT NULL,
    event_kind INTEGER NOT NULL,
    payload BLOB NOT NULL,
    checksum BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS events_run_sequence ON events(run_id, sequence);

CREATE TABLE IF NOT EXISTS snapshots (
    run_id TEXT NOT NULL,
    at_sequence INTEGER NOT NULL,
    state_blob BLOB NOT NULL,
    checksum BLOB NOT NULL,
    PRIMARY KEY (run_id, at_sequence)
);

CREATE TABLE IF NOT EXISTS artifacts (
    content_hash BLOB PRIMARY KEY,
    size_bytes INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    payload BLOB NOT NULL,
    checksum BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS branches (
    branch_id TEXT PRIMARY KEY,
    parent_run_id TEXT NOT NULL,
    fork_sequence INTEGER NOT NULL,
    replay_mode TEXT NOT NULL,
    created_at_ms TEXT NOT NULL
);
"#;
const SCHEMA_V2: &str = "ALTER TABLE artifacts ADD COLUMN trust_tag INTEGER NOT NULL DEFAULT 3;";

pub struct SqliteEventStore {
    connection: Connection,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let mut connection = Connection::open(path).map_err(database_error)?;
        configure_connection(&connection)?;
        migrate(&mut connection).map_err(StoreError::Corrupt)?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let mut connection = Connection::open_in_memory().map_err(database_error)?;
        configure_connection(&connection)?;
        migrate(&mut connection).map_err(StoreError::Corrupt)?;
        Ok(Self { connection })
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        self.connection
            .query_row("SELECT MAX(version) FROM schema_meta", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(database_error)?
            .ok_or_else(|| StoreError::Corrupt("schema has no version".to_string()))
    }

    pub fn recorded_replay(&self, run_id: RunId) -> Result<RecordedReplay, StoreError> {
        let events = self.events(run_id)?;
        let state = reconstruct_events(run_id, &events)?;
        Ok(RecordedReplay { events, state })
    }

    pub fn event_count(&self, run_id: RunId) -> Result<u64, StoreError> {
        let count: i64 = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM events WHERE run_id = ?1",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        u64::try_from(count).map_err(|_| StoreError::Corrupt("negative event count".to_string()))
    }

    pub fn append_trace(
        &mut self,
        trace: &orynth_kernel::EventTrace,
    ) -> Result<Vec<Sequence>, StoreError> {
        self.append_batch(trace.events())
    }
}

impl EventStore for SqliteEventStore {
    fn append(&mut self, event: Event) -> Result<Sequence, StoreError> {
        self.append_batch(std::slice::from_ref(&event))
            .map(|sequences| sequences[0])
    }

    fn append_batch(&mut self, events: &[Event]) -> Result<Vec<Sequence>, StoreError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let mut new_ids = std::collections::BTreeSet::new();
        let mut payloads = Vec::with_capacity(events.len());
        for event in events {
            if !new_ids.insert(event.id) {
                return Err(StoreError::DuplicateEvent(event.id));
            }
            payloads.push(encode_event(event).map_err(StoreError::Corrupt)?);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;

        for event in events {
            let duplicate: Option<String> = transaction
                .query_row(
                    "SELECT event_id FROM events WHERE event_id = ?1",
                    params![event.id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database_error)?;
            if duplicate.is_some() {
                return Err(StoreError::DuplicateEvent(event.id));
            }
        }

        let next: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let mut sequences = Vec::with_capacity(events.len());
        for (index, (event, payload)) in events.iter().zip(payloads).enumerate() {
            let offset = i64::try_from(index)
                .map_err(|_| StoreError::Corrupt("event sequence overflow".to_string()))?;
            let sequence = next
                .checked_add(offset)
                .ok_or_else(|| StoreError::Corrupt("event sequence overflow".to_string()))?;
            let checksum = DeterministicContentHasher.hash(&payload).as_bytes();
            transaction
                .execute(
                    "INSERT INTO events
                     (sequence, event_id, run_id, occurred_at_ms, event_kind, payload, checksum)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        sequence,
                        event.id.to_string(),
                        event.run_id.to_string(),
                        event.occurred_at_ms.to_string(),
                        event_kind_tag(&event.kind),
                        payload,
                        checksum.as_slice()
                    ],
                )
                .map_err(database_error)?;
            sequences.push(
                u64::try_from(sequence)
                    .map_err(|_| StoreError::Corrupt("event sequence overflow".to_string()))?,
            );
        }
        transaction.commit().map_err(database_error)?;
        Ok(sequences)
    }

    fn events(&self, run_id: RunId) -> Result<Vec<StoredEvent>, StoreError> {
        read_events(
            &self.connection,
            "SELECT sequence, event_id, run_id, payload, checksum
             FROM events WHERE run_id = ?1 ORDER BY sequence",
            params![run_id.to_string()],
        )
    }

    fn events_since(
        &self,
        run_id: RunId,
        sequence: Sequence,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let sequence = i64::try_from(sequence)
            .map_err(|_| StoreError::Corrupt("event sequence overflow".to_string()))?;
        read_events(
            &self.connection,
            "SELECT sequence, event_id, run_id, payload, checksum
             FROM events WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence",
            params![run_id.to_string(), sequence],
        )
    }

    fn reconstruct(&self, run_id: RunId) -> Result<RuntimeState, StoreError> {
        let events = self.events(run_id)?;
        reconstruct_events(run_id, &events)
    }

    fn snapshot(&self, run_id: RunId) -> Result<RuntimeSnapshot, StoreError> {
        let events = self.events(run_id)?;
        let state = reconstruct_events(run_id, &events)?;
        let at_sequence = events
            .last()
            .map(|event| event.sequence)
            .ok_or(StoreError::UnknownRun(run_id))?;
        Ok(RuntimeSnapshot {
            run_id,
            at_sequence,
            state,
        })
    }
}

impl BranchStore for SqliteEventStore {
    fn create_branch(
        &mut self,
        parent_run_id: RunId,
        fork_sequence: Sequence,
        replay_mode: ReplayMode,
        created_at_ms: u64,
    ) -> Result<BranchMetadata, StoreError> {
        let fork_sequence = i64::try_from(fork_sequence)
            .map_err(|_| StoreError::Corrupt("fork sequence overflow".to_string()))?;
        let created_at_ms = i64::try_from(created_at_ms)
            .map_err(|_| StoreError::Corrupt("branch timestamp overflow".to_string()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;

        let fork_exists: Option<i64> = transaction
            .query_row(
                "SELECT sequence FROM events WHERE run_id = ?1 AND sequence = ?2",
                params![parent_run_id.to_string(), fork_sequence],
                |row| row.get(0),
            )
            .optional()
            .map_err(database_error)?;
        if fork_exists.is_none() {
            let parent_exists: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM events WHERE run_id = ?1 LIMIT 1",
                    params![parent_run_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database_error)?;
            if parent_exists.is_none() {
                return Err(StoreError::UnknownRun(parent_run_id));
            }
            return Err(StoreError::InvalidTransition(format!(
                "fork sequence {fork_sequence} is not present in run {parent_run_id}"
            )));
        }

        let metadata = BranchMetadata {
            branch_id: BranchId::new(),
            parent_run_id,
            fork_sequence: u64::try_from(fork_sequence)
                .map_err(|_| StoreError::Corrupt("negative fork sequence".to_string()))?,
            replay_mode,
            created_at_ms: u64::try_from(created_at_ms)
                .map_err(|_| StoreError::Corrupt("negative branch timestamp".to_string()))?,
        };
        transaction
            .execute(
                "INSERT INTO branches
                 (branch_id, parent_run_id, fork_sequence, replay_mode, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    metadata.branch_id.to_string(),
                    metadata.parent_run_id.to_string(),
                    fork_sequence,
                    metadata.replay_mode.as_str(),
                    created_at_ms
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(metadata)
    }

    fn branch(&self, branch_id: BranchId) -> Result<Option<BranchMetadata>, StoreError> {
        let row: Option<(String, String, i64, String, String)> = self
            .connection
            .query_row(
                "SELECT branch_id, parent_run_id, fork_sequence, replay_mode, created_at_ms
                 FROM branches WHERE branch_id = ?1",
                params![branch_id.to_string()],
                branch_columns,
            )
            .optional()
            .map_err(database_error)?;
        row.map(decode_branch).transpose()
    }

    fn branches_for_run(&self, parent_run_id: RunId) -> Result<Vec<BranchMetadata>, StoreError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT branch_id, parent_run_id, fork_sequence, replay_mode, created_at_ms
                 FROM branches WHERE parent_run_id = ?1 ORDER BY created_at_ms, branch_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![parent_run_id.to_string()], branch_columns)
            .map_err(database_error)?;
        rows.map(|row| {
            let values = row.map_err(database_error)?;
            decode_branch(values)
        })
        .collect::<Result<Vec<_>, _>>()
    }
}

impl SnapshotStore for SqliteEventStore {
    fn save_snapshot(&mut self, snapshot: &RuntimeSnapshot) -> Result<(), StoreError> {
        let events = self.events(snapshot.run_id)?;
        validate_snapshot(&events, snapshot)?;
        let state_blob = encode_snapshot_state(&snapshot.state).map_err(StoreError::Corrupt)?;
        let checksum = DeterministicContentHasher.hash(&state_blob).as_bytes();
        let at_sequence = i64::try_from(snapshot.at_sequence)
            .map_err(|_| StoreError::Corrupt("snapshot sequence overflow".to_string()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;

        let existing: Option<(Vec<u8>, Vec<u8>)> = transaction
            .query_row(
                "SELECT state_blob, checksum FROM snapshots
                 WHERE run_id = ?1 AND at_sequence = ?2",
                params![snapshot.run_id.to_string(), at_sequence],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        if let Some((existing_blob, existing_checksum)) = existing {
            if existing_checksum.as_slice()
                != DeterministicContentHasher.hash(&existing_blob).as_bytes()
            {
                return Err(StoreError::Corrupt(format!(
                    "snapshot checksum mismatch for run {} at sequence {}",
                    snapshot.run_id, snapshot.at_sequence
                )));
            }
            if existing_blob != state_blob || existing_checksum.as_slice() != checksum {
                return Err(StoreError::InvalidTransition(format!(
                    "snapshot {} at sequence {} is immutable",
                    snapshot.run_id, snapshot.at_sequence
                )));
            }
            transaction.commit().map_err(database_error)?;
            return Ok(());
        }

        transaction
            .execute(
                "INSERT INTO snapshots (run_id, at_sequence, state_blob, checksum)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    snapshot.run_id.to_string(),
                    at_sequence,
                    state_blob,
                    checksum.as_slice()
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn load_snapshot(
        &self,
        run_id: RunId,
        at_or_before: Option<Sequence>,
    ) -> Result<Option<RuntimeSnapshot>, StoreError> {
        let row: Option<(i64, Vec<u8>, Vec<u8>)> = if let Some(at_or_before) = at_or_before {
            let at_or_before = i64::try_from(at_or_before)
                .map_err(|_| StoreError::Corrupt("snapshot sequence overflow".to_string()))?;
            self.connection
                .query_row(
                    "SELECT at_sequence, state_blob, checksum FROM snapshots
                     WHERE run_id = ?1 AND at_sequence <= ?2
                     ORDER BY at_sequence DESC LIMIT 1",
                    params![run_id.to_string(), at_or_before],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(database_error)?
        } else {
            self.connection
                .query_row(
                    "SELECT at_sequence, state_blob, checksum FROM snapshots
                     WHERE run_id = ?1 ORDER BY at_sequence DESC LIMIT 1",
                    params![run_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(database_error)?
        };

        let Some((at_sequence, state_blob, checksum)) = row else {
            return Ok(None);
        };
        if checksum.as_slice() != DeterministicContentHasher.hash(&state_blob).as_bytes() {
            return Err(StoreError::Corrupt(format!(
                "snapshot checksum mismatch for run {run_id} at sequence {at_sequence}"
            )));
        }
        let snapshot = RuntimeSnapshot {
            run_id,
            at_sequence: u64::try_from(at_sequence)
                .map_err(|_| StoreError::Corrupt("negative snapshot sequence".to_string()))?,
            state: decode_snapshot_state(&state_blob).map_err(StoreError::Corrupt)?,
        };
        let events = self.events(run_id)?;
        validate_snapshot(&events, &snapshot)?;
        Ok(Some(snapshot))
    }
}

pub struct SqliteArtifactStore {
    connection: Connection,
}

type ArtifactRow = (i64, String, Vec<u8>, Vec<u8>, i64);

impl SqliteArtifactStore {
    pub fn schema_version(&self) -> Result<i64, ArtifactError> {
        self.connection
            .query_row("SELECT MAX(version) FROM schema_meta", [], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(artifact_database_error)?
            .ok_or_else(|| ArtifactError::Corrupt("schema has no version".to_string()))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| ArtifactError::Storage(error.to_string()))?;
        }
        let mut connection = Connection::open(path).map_err(artifact_database_error)?;
        configure_connection(&connection)
            .map_err(|error| ArtifactError::Storage(error.to_string()))?;
        migrate(&mut connection).map_err(ArtifactError::Corrupt)?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self, ArtifactError> {
        let mut connection = Connection::open_in_memory().map_err(artifact_database_error)?;
        configure_connection(&connection)
            .map_err(|error| ArtifactError::Storage(error.to_string()))?;
        migrate(&mut connection).map_err(ArtifactError::Corrupt)?;
        Ok(Self { connection })
    }
}

impl ArtifactStore for SqliteArtifactStore {
    fn put_with_trust(
        &mut self,
        media_type: &str,
        bytes: Vec<u8>,
        trust: TrustOrigin,
    ) -> Result<ArtifactRef, ArtifactError> {
        if media_type.trim().is_empty() {
            return Err(ArtifactError::InvalidMediaType);
        }

        let content_hash = DeterministicContentHasher.hash(&bytes);
        let reference = ArtifactRef {
            content_hash,
            size_bytes: bytes.len() as u64,
            media_type: media_type.to_string(),
            trust,
        };
        let digest = content_hash.as_bytes();
        let checksum = DeterministicContentHasher.hash(&bytes).as_bytes();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(artifact_database_error)?;

        let existing: Option<ArtifactRow> = transaction
            .query_row(
                "SELECT size_bytes, media_type, payload, checksum, trust_tag
                 FROM artifacts WHERE content_hash = ?1",
                params![digest.as_slice()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(artifact_database_error)?;

        if let Some((
            size_bytes,
            existing_media_type,
            existing_bytes,
            existing_checksum,
            existing_trust,
        )) = existing
        {
            if existing_checksum.as_slice()
                != DeterministicContentHasher.hash(&existing_bytes).as_bytes()
            {
                return Err(ArtifactError::Corrupt(
                    "stored artifact checksum mismatch".to_string(),
                ));
            }
            if existing_bytes != bytes {
                return Err(ArtifactError::HashCollision(content_hash));
            }
            if existing_media_type != media_type || size_bytes != bytes.len() as i64 {
                return Err(ArtifactError::MetadataMismatch(content_hash));
            }
            let existing_trust = decode_artifact_trust(existing_trust)?;
            let combined = existing_trust.combine(reference.trust);
            if existing_trust != combined {
                transaction
                    .execute(
                        "UPDATE artifacts SET trust_tag = ?1 WHERE content_hash = ?2",
                        params![artifact_trust_tag(combined), digest.as_slice()],
                    )
                    .map_err(artifact_database_error)?;
            }
            transaction.commit().map_err(artifact_database_error)?;
            return Ok(ArtifactRef {
                trust: combined,
                ..reference
            });
        }

        transaction
            .execute(
                "INSERT INTO artifacts
                 (content_hash, size_bytes, media_type, payload, checksum, trust_tag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    digest.as_slice(),
                    i64::try_from(bytes.len())
                        .map_err(|_| ArtifactError::Storage("artifact too large".to_string()))?,
                    media_type,
                    bytes,
                    checksum.as_slice(),
                    artifact_trust_tag(reference.trust)
                ],
            )
            .map_err(artifact_database_error)?;
        transaction.commit().map_err(artifact_database_error)?;
        Ok(reference)
    }

    fn get(&self, content_hash: ContentHash) -> Result<Option<StoredArtifact>, ArtifactError> {
        let digest = content_hash.as_bytes();
        let row: Option<ArtifactRow> = self
            .connection
            .query_row(
                "SELECT size_bytes, media_type, payload, checksum, trust_tag
                 FROM artifacts WHERE content_hash = ?1",
                params![digest.as_slice()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(artifact_database_error)?;

        let Some((size_bytes, media_type, bytes, checksum, trust_tag)) = row else {
            return Ok(None);
        };
        if checksum.as_slice() != DeterministicContentHasher.hash(&bytes).as_bytes() {
            return Err(ArtifactError::Corrupt(
                "stored artifact checksum mismatch".to_string(),
            ));
        }
        if size_bytes != bytes.len() as i64 {
            return Err(ArtifactError::Corrupt(
                "stored artifact size mismatch".to_string(),
            ));
        }
        if DeterministicContentHasher.hash(&bytes) != content_hash {
            return Err(ArtifactError::Corrupt(
                "stored artifact content hash mismatch".to_string(),
            ));
        }

        Ok(Some(StoredArtifact {
            reference: ArtifactRef {
                content_hash,
                size_bytes: u64::try_from(size_bytes)
                    .map_err(|_| ArtifactError::Corrupt("negative artifact size".to_string()))?,
                media_type,
                trust: decode_artifact_trust(trust_tag)?,
            },
            bytes: bytes.to_vec(),
        }))
    }
}

fn artifact_trust_tag(trust: TrustOrigin) -> i64 {
    match trust {
        TrustOrigin::Runtime => 0,
        TrustOrigin::TrustedProject => 1,
        TrustOrigin::UserProvided => 2,
        TrustOrigin::Generated => 3,
        TrustOrigin::RemoteAgent => 4,
        TrustOrigin::External => 5,
        TrustOrigin::WebUntrusted => 6,
        TrustOrigin::McpMetadata => 7,
        TrustOrigin::McpResult => 8,
    }
}

fn decode_artifact_trust(tag: i64) -> Result<TrustOrigin, ArtifactError> {
    match tag {
        0 => Ok(TrustOrigin::Runtime),
        1 => Ok(TrustOrigin::TrustedProject),
        2 => Ok(TrustOrigin::UserProvided),
        3 => Ok(TrustOrigin::Generated),
        4 => Ok(TrustOrigin::RemoteAgent),
        5 => Ok(TrustOrigin::External),
        6 => Ok(TrustOrigin::WebUntrusted),
        7 => Ok(TrustOrigin::McpMetadata),
        8 => Ok(TrustOrigin::McpResult),
        _ => Err(ArtifactError::Corrupt(format!(
            "unknown artifact trust tag {tag}"
        ))),
    }
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(database_error)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
        .map_err(database_error)?;
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                version INTEGER PRIMARY KEY,
                checksum BLOB NOT NULL
            );",
        )
        .map_err(|error| error.to_string())?;

    let current: Option<i64> = transaction
        .query_row("SELECT MAX(version) FROM schema_meta", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map_err(|error| error.to_string())?;
    if current.is_some_and(|version| version > SCHEMA_VERSION) {
        return Err("database schema is newer than this runtime".to_string());
    }

    let v1_checksum = DeterministicContentHasher
        .hash(SCHEMA_V1.as_bytes())
        .as_bytes();
    let v2_checksum = DeterministicContentHasher
        .hash(SCHEMA_V2.as_bytes())
        .as_bytes();

    match current {
        None => {
            transaction
                .execute_batch(SCHEMA_V1)
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO schema_meta(version, checksum) VALUES (?1, ?2)",
                    params![1_i64, v1_checksum.as_slice()],
                )
                .map_err(|error| error.to_string())?;
            transaction
                .execute_batch(SCHEMA_V2)
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO schema_meta(version, checksum) VALUES (?1, ?2)",
                    params![2_i64, v2_checksum.as_slice()],
                )
                .map_err(|error| error.to_string())?;
        }
        Some(1) => {
            let stored: Vec<u8> = transaction
                .query_row(
                    "SELECT checksum FROM schema_meta WHERE version = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if stored.as_slice() != v1_checksum {
                return Err("schema migration checksum mismatch".to_string());
            }
            transaction
                .execute_batch(SCHEMA_V2)
                .map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO schema_meta(version, checksum) VALUES (?1, ?2)",
                    params![2_i64, v2_checksum.as_slice()],
                )
                .map_err(|error| error.to_string())?;
        }
        Some(2) => {
            let stored: Vec<u8> = transaction
                .query_row(
                    "SELECT checksum FROM schema_meta WHERE version = 2",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if stored.as_slice() != v2_checksum {
                return Err("schema migration checksum mismatch".to_string());
            }
            transaction
                .execute_batch(SCHEMA_V1)
                .map_err(|error| error.to_string())?;
        }
        Some(version) => {
            return Err(format!("unsupported schema version {version}"));
        }
    }

    transaction.commit().map_err(|error| error.to_string())
}

fn read_events<P: Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> Result<Vec<StoredEvent>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(database_error)?;
    let mut rows = statement.query(parameters).map_err(database_error)?;
    let mut events = Vec::new();

    while let Some(row) = rows.next().map_err(database_error)? {
        let sequence: i64 = row.get(0).map_err(database_error)?;
        let stored_event_id: String = row.get(1).map_err(database_error)?;
        let stored_run_id: String = row.get(2).map_err(database_error)?;
        let payload: Vec<u8> = row.get(3).map_err(database_error)?;
        let stored_checksum: Vec<u8> = row.get(4).map_err(database_error)?;
        let actual_checksum = DeterministicContentHasher.hash(&payload).as_bytes();
        if stored_checksum.as_slice() != actual_checksum {
            return Err(StoreError::Corrupt(format!(
                "SQLite event checksum mismatch at sequence {sequence}"
            )));
        }

        let event = decode_event(&payload).map_err(StoreError::Corrupt)?;
        if event.id.to_string() != stored_event_id || event.run_id.to_string() != stored_run_id {
            return Err(StoreError::Corrupt(format!(
                "SQLite event identity mismatch at sequence {sequence}"
            )));
        }
        events.push(StoredEvent {
            sequence: u64::try_from(sequence)
                .map_err(|_| StoreError::Corrupt("negative event sequence".to_string()))?,
            event,
        });
    }
    Ok(events)
}

fn branch_columns(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, i64, String, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_branch(
    (branch_id, parent_run_id, fork_sequence, replay_mode, created_at_ms): (
        String,
        String,
        i64,
        String,
        String,
    ),
) -> Result<BranchMetadata, StoreError> {
    Ok(BranchMetadata {
        branch_id: BranchId::from_u64(parse_id_value(&branch_id, "branch-")?),
        parent_run_id: RunId::from_u64(parse_id_value(&parent_run_id, "run-")?),
        fork_sequence: u64::try_from(fork_sequence)
            .map_err(|_| StoreError::Corrupt("negative fork sequence".to_string()))?,
        replay_mode: ReplayMode::parse(&replay_mode)?,
        created_at_ms: created_at_ms
            .parse::<u64>()
            .map_err(|_| StoreError::Corrupt("invalid branch timestamp".to_string()))?,
    })
}

fn parse_id_value(value: &str, prefix: &str) -> Result<u64, StoreError> {
    let digits = value
        .strip_prefix(prefix)
        .ok_or_else(|| StoreError::Corrupt(format!("invalid persisted ID {value}")))?;
    u64::from_str_radix(digits, 16)
        .map_err(|_| StoreError::Corrupt(format!("invalid persisted ID {value}")))
}

const SNAPSHOT_MAGIC_V1: &[u8] = b"ORYNTH-SNAPSHOT-1\n";
const SNAPSHOT_MAGIC_V2: &[u8] = b"ORYNTH-SNAPSHOT-2\n";
const SNAPSHOT_MAGIC_V3: &[u8] = b"ORYNTH-SNAPSHOT-3\n";
const MAX_SNAPSHOT_ITEMS: u32 = 1_000_000;

#[derive(Clone, Copy)]
struct SnapshotFeatures {
    has_cache_usage: bool,
    has_artifact_trust: bool,
}

pub(crate) fn encode_snapshot_state(state: &RuntimeState) -> Result<Vec<u8>, String> {
    let mut writer = SnapshotWriter::new();
    writer.bytes.extend_from_slice(SNAPSHOT_MAGIC_V3);
    writer.u64(state.run_id.value());
    writer.u8(run_status_tag(state.status));
    writer.u64(state.events_applied);

    writer.count(state.tasks.len())?;
    for task in state.tasks.values() {
        writer.u64(task.id.value());
        writer.u64(task.run_id.value());
        writer.string(&task.title)?;
        writer.u8(task_status_tag(task.status));
    }

    writer.count(state.agents.len())?;
    for agent in state.agents.values() {
        writer.u64(agent.identity.id.value());
        writer.string(&agent.identity.name)?;
        writer.string(&agent.identity.mission)?;
        encode_model(&mut writer, &agent.identity.model)?;
        writer.u8(agent_status_tag(agent.status));
        writer.u32(agent.chunks_received);
        writer.u64(agent.usage.input_tokens);
        writer.u64(agent.usage.output_tokens);
        match agent.usage.cached_input_tokens {
            Some(cached_input_tokens) => {
                writer.u8(1);
                writer.u64(cached_input_tokens);
            }
            None => writer.u8(0),
        }
    }

    writer.count(state.artifacts.len())?;
    for artifact in state.artifacts.values() {
        writer.array(&artifact.content_hash.as_bytes());
        writer.u64(artifact.size_bytes);
        writer.string(&artifact.media_type)?;
        writer.u8(artifact_trust_tag(artifact.trust) as u8);
    }
    Ok(writer.bytes)
}

pub(crate) fn decode_snapshot_state(bytes: &[u8]) -> Result<RuntimeState, String> {
    let mut reader = SnapshotReader::new(bytes);
    let features = reader.snapshot_version()?;
    let run_id = RunId::from_u64(reader.u64()?);
    let status = decode_run_status(reader.u8()?)?;
    let events_applied = reader.u64()?;

    let mut tasks = BTreeMap::new();
    for _ in 0..reader.count()? {
        let task_id = TaskId::from_u64(reader.u64()?);
        let task = TaskState {
            id: task_id,
            run_id: RunId::from_u64(reader.u64()?),
            title: reader.string()?,
            status: decode_task_status(reader.u8()?)?,
        };
        if tasks.insert(task_id, task).is_some() {
            return Err(format!("duplicate task {task_id} in snapshot"));
        }
    }

    let mut agents = BTreeMap::new();
    for _ in 0..reader.count()? {
        let agent_id = AgentId::from_u64(reader.u64()?);
        let identity = AgentIdentity {
            id: agent_id,
            name: reader.string()?,
            mission: reader.string()?,
            model: decode_model(&mut reader)?,
        };
        let model = identity.model.clone();
        let status = decode_agent_status(reader.u8()?)?;
        let chunks_received = reader.u32()?;
        let input_tokens = reader.u64()?;
        let output_tokens = reader.u64()?;
        let cached_input_tokens = if features.has_cache_usage {
            match reader.u8()? {
                0 => None,
                1 => Some(reader.u64()?),
                tag => return Err(format!("unknown cached usage tag {tag}")),
            }
        } else {
            None
        };
        let usage = cached_input_tokens.map_or_else(
            || Usage::new(input_tokens, output_tokens),
            |cached| Usage::new(input_tokens, output_tokens).with_cached_input_tokens(cached),
        );
        let agent = AgentState {
            model,
            identity,
            status,
            chunks_received,
            usage,
        };
        if agents.insert(agent_id, agent).is_some() {
            return Err(format!("duplicate agent {agent_id} in snapshot"));
        }
    }

    let mut artifacts = BTreeMap::new();
    for _ in 0..reader.count()? {
        let content_hash = ContentHash::from_digest(reader.array()?);
        let size_bytes = reader.u64()?;
        let media_type = reader.string()?;
        let trust = if features.has_artifact_trust {
            decode_artifact_trust(reader.u8()? as i64).map_err(|error| error.to_string())?
        } else {
            TrustOrigin::Generated
        };
        let reference = ArtifactRef {
            content_hash,
            size_bytes,
            media_type,
            trust,
        };
        if artifacts.insert(content_hash, reference).is_some() {
            return Err(format!("duplicate artifact {content_hash} in snapshot"));
        }
    }
    reader.finish()?;

    Ok(RuntimeState {
        run_id,
        status,
        tasks,
        agents,
        artifacts,
        events_applied,
    })
}

fn encode_model(writer: &mut SnapshotWriter, model: &ModelRef) -> Result<(), String> {
    writer.string(&model.provider)?;
    writer.string(&model.model)?;
    match &model.class {
        ModelClass::Local => writer.u8(0),
        ModelClass::Cheap => writer.u8(1),
        ModelClass::Strong => writer.u8(2),
        ModelClass::Custom(value) => {
            writer.u8(3);
            writer.string(value)?;
        }
    }
    Ok(())
}

fn decode_model(reader: &mut SnapshotReader<'_>) -> Result<ModelRef, String> {
    let provider = reader.string()?;
    let model = reader.string()?;
    let class = match reader.u8()? {
        0 => ModelClass::Local,
        1 => ModelClass::Cheap,
        2 => ModelClass::Strong,
        3 => ModelClass::Custom(reader.string()?),
        tag => return Err(format!("unknown model class tag {tag}")),
    };
    Ok(ModelRef {
        provider,
        model,
        class,
    })
}

fn run_status_tag(status: RunStatus) -> u8 {
    match status {
        RunStatus::Active => 0,
        RunStatus::Completed => 1,
        RunStatus::Cancelled => 2,
        RunStatus::Failed => 3,
    }
}

fn decode_run_status(tag: u8) -> Result<RunStatus, String> {
    match tag {
        0 => Ok(RunStatus::Active),
        1 => Ok(RunStatus::Completed),
        2 => Ok(RunStatus::Cancelled),
        3 => Ok(RunStatus::Failed),
        tag => Err(format!("unknown run status tag {tag}")),
    }
}

fn task_status_tag(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::Created => 0,
    }
}

fn decode_task_status(tag: u8) -> Result<TaskStatus, String> {
    match tag {
        0 => Ok(TaskStatus::Created),
        tag => Err(format!("unknown task status tag {tag}")),
    }
}

fn agent_status_tag(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Created => 0,
        AgentStatus::Running => 1,
        AgentStatus::Completed => 2,
        AgentStatus::Cancelled => 3,
        AgentStatus::Failed => 4,
        AgentStatus::Paused => 5,
    }
}

fn decode_agent_status(tag: u8) -> Result<AgentStatus, String> {
    match tag {
        0 => Ok(AgentStatus::Created),
        1 => Ok(AgentStatus::Running),
        2 => Ok(AgentStatus::Completed),
        3 => Ok(AgentStatus::Cancelled),
        4 => Ok(AgentStatus::Failed),
        5 => Ok(AgentStatus::Paused),
        tag => Err(format!("unknown agent status tag {tag}")),
    }
}

struct SnapshotWriter {
    bytes: Vec<u8>,
}

impl SnapshotWriter {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn array(&mut self, value: &[u8; 32]) {
        self.bytes.extend_from_slice(value);
    }

    fn count(&mut self, value: usize) -> Result<(), String> {
        self.u32(u32::try_from(value).map_err(|_| "snapshot collection is too large".to_string())?);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), String> {
        let bytes = value.as_bytes();
        self.u32(
            u32::try_from(bytes.len()).map_err(|_| "snapshot string is too large".to_string())?,
        );
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct SnapshotReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SnapshotReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "snapshot offset overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("snapshot is truncated".to_string());
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn snapshot_version(&mut self) -> Result<SnapshotFeatures, String> {
        if self.bytes.starts_with(SNAPSHOT_MAGIC_V3) {
            self.offset += SNAPSHOT_MAGIC_V3.len();
            return Ok(SnapshotFeatures {
                has_cache_usage: true,
                has_artifact_trust: true,
            });
        }
        if self.bytes.starts_with(SNAPSHOT_MAGIC_V2) {
            self.offset += SNAPSHOT_MAGIC_V2.len();
            return Ok(SnapshotFeatures {
                has_cache_usage: true,
                has_artifact_trust: false,
            });
        }
        if self.bytes.starts_with(SNAPSHOT_MAGIC_V1) {
            self.offset += SNAPSHOT_MAGIC_V1.len();
            return Ok(SnapshotFeatures {
                has_cache_usage: false,
                has_artifact_trust: false,
            });
        }
        Err("snapshot magic is invalid".to_string())
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("validated u32 width"),
        ))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("validated u64 width"),
        ))
    }

    fn array(&mut self) -> Result<[u8; 32], String> {
        Ok(self.take(32)?.try_into().expect("validated hash width"))
    }

    fn count(&mut self) -> Result<usize, String> {
        let count = self.u32()?;
        if count > MAX_SNAPSHOT_ITEMS {
            return Err(format!("snapshot collection count {count} is too large"));
        }
        usize::try_from(count).map_err(|_| "snapshot collection count overflows usize".to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| "snapshot string length overflows usize".to_string())?;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| "snapshot contains invalid UTF-8".to_string())
    }

    fn finish(&self) -> Result<(), String> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("snapshot has trailing bytes".to_string())
        }
    }
}

fn reconstruct_events(run_id: RunId, events: &[StoredEvent]) -> Result<RuntimeState, StoreError> {
    if events.is_empty() {
        return Err(StoreError::UnknownRun(run_id));
    }
    let mut memory = InMemoryEventStore::new();
    for stored in events {
        memory.append(stored.event.clone())?;
    }
    memory.reconstruct(run_id)
}

fn event_kind_tag(kind: &EventKind) -> i64 {
    match kind {
        EventKind::RunCreated { .. } => 0,
        EventKind::TaskCreated { .. } => 1,
        EventKind::AgentCreated { .. } => 2,
        EventKind::ModelRequested { .. } => 3,
        EventKind::ModelChunkReceived { .. } => 4,
        EventKind::ModelCompleted { .. } => 5,
        EventKind::ModelCancelled { .. } => 6,
        EventKind::ModelFailed { .. } => 7,
        EventKind::RunCompleted { .. } => 8,
        EventKind::RunCancelled { .. } => 9,
        EventKind::RunFailed { .. } => 10,
        EventKind::ArtifactCreated { .. } => 11,
        EventKind::ContextTransition { .. } => 12,
        EventKind::CacheObserved { .. } => 13,
        EventKind::AgentMessage { .. } => 14,
        EventKind::AssumptionTransition { .. } => 15,
        EventKind::SchedulerTransition { .. } => 16,
        EventKind::AgentPaused { .. } => 17,
        EventKind::AgentResumed { .. } => 18,
        EventKind::CapabilityTransition { .. } => 19,
        EventKind::ToolTransition { .. } => 20,
        EventKind::FailureMemoryTransition { .. } => 21,
        EventKind::SpecialistTransition { .. } => 22,
    }
}

fn database_error(error: rusqlite::Error) -> StoreError {
    StoreError::Storage(error.to_string())
}

fn artifact_database_error(error: rusqlite::Error) -> ArtifactError {
    ArtifactError::Storage(error.to_string())
}

fn storage_error(error: std::io::Error) -> StoreError {
    StoreError::Storage(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ForkStore;
    use orynth_agent::AgentSession;
    use orynth_kernel::{AgentIdentity, CancellationToken, EventTrace, ModelClass, ModelRef};
    use orynth_provider::MockProvider;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_path(label: &str) -> PathBuf {
        let number = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "orynth-sqlite-{label}-{}-{number}.db",
            std::process::id()
        ))
    }

    fn successful_trace() -> EventTrace {
        let model = ModelRef::new("mock", "sqlite", ModelClass::Cheap);
        let identity = AgentIdentity::new("sqlite-agent", "test sqlite", model.clone());
        AgentSession::new(identity, MockProvider::new(model, "sqlite response"))
            .run("persist this", CancellationToken::new())
            .expect("mock run should succeed")
            .trace
    }

    fn run_id(trace: &EventTrace) -> RunId {
        match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        }
    }

    #[test]
    fn sqlite_event_store_reopens_and_reconstructs() {
        let path = temp_path("events");
        let trace = successful_trace();
        let run_id = run_id(&trace);

        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            assert_eq!(store.schema_version().expect("schema version"), 2);
            store.append_trace(&trace).expect("trace should append");
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        let state = reopened
            .reconstruct(run_id)
            .expect("state should reconstruct");
        assert_eq!(state.status, super::super::RunStatus::Completed);
        assert_eq!(
            reopened.event_count(run_id).expect("event count"),
            trace.len() as u64
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_preserves_opaque_context_transition_payloads() {
        let path = temp_path("context-events");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::ContextTransition {
                version: 1,
                payload: vec![5, 6, 7],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            store
                .append_trace(&trace)
                .expect("context event should persist");
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::ContextTransition { version: 1, payload } if payload == &[5, 6, 7]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            super::super::RunStatus::Completed
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_preserves_opaque_specialist_transition_payloads() {
        let path = temp_path("specialist-events");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::SpecialistTransition {
                version: 1,
                payload: vec![6, 5, 4],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            store
                .append_trace(&trace)
                .expect("specialist event should persist");
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::SpecialistTransition { version: 1, payload }
                if payload == &[6, 5, 4]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            super::super::RunStatus::Completed
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_migration_is_idempotent_and_duplicate_append_is_atomic() {
        let path = temp_path("migration");
        let trace = successful_trace();
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
        store
            .append(trace.events()[0].clone())
            .expect("first event");
        let duplicate = store.append(trace.events()[0].clone());
        assert!(matches!(duplicate, Err(StoreError::DuplicateEvent(_))));
        assert_eq!(store.event_count(run_id(&trace)).expect("event count"), 1);

        drop(store);
        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert_eq!(reopened.schema_version().expect("schema version"), 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_batch_append_is_atomic_on_duplicate_ids() {
        let path = temp_path("batch-atomic");
        let trace = successful_trace();
        let run_id = run_id(&trace);
        let event = trace.events()[0].clone();
        let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");

        assert!(matches!(
            store.append_batch(&[event.clone(), event]),
            Err(StoreError::DuplicateEvent(_))
        ));
        assert_eq!(store.event_count(run_id).expect("event count"), 0);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_writers_serialize_across_connections() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let path = temp_path("concurrent-writers");
        let traces = [successful_trace(), successful_trace()];
        let barrier = Arc::new(Barrier::new(traces.len()));
        let handles = traces
            .into_iter()
            .map(|trace| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let mut store = SqliteEventStore::open(&path)?;
                    store.append_trace(&trace)
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer thread should finish"))
            .collect::<Result<Vec<_>, _>>()
            .expect("concurrent appends should succeed");
        assert!(results.iter().all(|sequences| !sequences.is_empty()));

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        let count: u64 = reopened
            .connection
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .expect("event count should query");
        assert_eq!(count, results.iter().map(Vec::len).sum::<usize>() as u64);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_branches_validate_and_survive_reopen() {
        let path = temp_path("branches");
        let trace = successful_trace();
        let run_id = run_id(&trace);
        let branch;
        let child_run_id = RunId::new();
        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            store.append_trace(&trace).expect("trace should append");
            branch = store
                .create_branch(run_id, 2, ReplayMode::ReexecuteLive, 9876)
                .expect("branch should be created");
            assert_eq!(
                store.branch(branch.branch_id).expect("branch lookup"),
                Some(branch.clone())
            );
            assert_eq!(
                store.branches_for_run(run_id).expect("branch list"),
                vec![branch.clone()]
            );
            let fork = store
                .materialize_fork(branch.branch_id, child_run_id)
                .expect("fork should materialize");
            assert_eq!(fork.copied_event_count, 2);
            assert_eq!(
                store
                    .reconstruct(child_run_id)
                    .expect("child should reconstruct")
                    .tasks
                    .len(),
                1
            );
            assert!(matches!(
                store.create_branch(run_id, 0, ReplayMode::Recorded, 9877),
                Err(StoreError::InvalidTransition(_))
            ));
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert_eq!(
            reopened
                .branch(branch.branch_id)
                .expect("branch lookup after reopen"),
            Some(branch)
        );
        assert_eq!(
            reopened
                .reconstruct(child_run_id)
                .expect("child should reconstruct after reopen")
                .status,
            RunStatus::Active
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_snapshots_encode_validate_and_survive_reopen() {
        let path = temp_path("snapshots");
        let trace = successful_trace();
        let run_id = run_id(&trace);
        let snapshot;
        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            store.append_trace(&trace).expect("trace should append");
            snapshot = store.snapshot(run_id).expect("snapshot should compute");
            store
                .save_snapshot(&snapshot)
                .expect("snapshot should persist");
            assert_eq!(
                store
                    .load_snapshot(run_id, Some(snapshot.at_sequence))
                    .expect("snapshot should load"),
                Some(snapshot.clone())
            );
            store
                .save_snapshot(&snapshot)
                .expect("same immutable snapshot should be idempotent");
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert_eq!(
            reopened
                .load_snapshot(run_id, None)
                .expect("snapshot should load after reopen"),
            Some(snapshot)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_snapshots_preserve_explicit_cache_usage_metadata() {
        let path = temp_path("cached-usage-snapshot");
        let run_id = RunId::new();
        let model = ModelRef::new("mock", "cached", ModelClass::Cheap);
        let identity = AgentIdentity::new("cached-agent", "test cache metadata", model);
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::AgentCreated {
                agent: identity.clone(),
            },
        ));
        trace.record(Event::new(
            run_id,
            EventKind::ModelCompleted {
                agent_id: identity.id,
                usage: Usage::new(20, 4).with_cached_input_tokens(12),
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        let snapshot;
        {
            let mut store = SqliteEventStore::open(&path).expect("SQLite store should open");
            store.append_trace(&trace).expect("trace should append");
            snapshot = store.snapshot(run_id).expect("snapshot should compute");
            store
                .save_snapshot(&snapshot)
                .expect("snapshot should persist");
        }

        let reopened = SqliteEventStore::open(&path).expect("SQLite store should reopen");
        assert_eq!(
            reopened
                .load_snapshot(run_id, None)
                .expect("snapshot should load")
                .expect("snapshot should exist")
                .state
                .agents
                .get(&identity.id)
                .expect("agent should exist")
                .usage
                .cached_input_tokens,
            Some(12)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn sqlite_artifacts_reopen_and_validate() {
        let path = temp_path("artifacts");
        let reference;
        {
            let mut store = SqliteArtifactStore::open(&path).expect("artifact store should open");
            reference = store
                .put_with_trust(
                    "text/plain",
                    b"sqlite artifact".to_vec(),
                    TrustOrigin::RemoteAgent,
                )
                .expect("artifact should store");
        }

        let reopened = SqliteArtifactStore::open(&path).expect("artifact store should reopen");
        let stored = reopened
            .get(reference.content_hash)
            .expect("artifact lookup")
            .expect("artifact should exist");
        assert_eq!(stored.reference, reference);
        assert_eq!(stored.reference.trust, TrustOrigin::RemoteAgent);
        assert_eq!(stored.bytes(), b"sqlite artifact");

        let _ = fs::remove_file(path);
    }
}
