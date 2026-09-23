use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use super::sqlite::{decode_snapshot_state, encode_snapshot_state};
use super::*;

const EVENT_MAGIC: &[u8] = b"ORYNTH-EVENT-1\n";
const METADATA_MAGIC: &[u8] = b"ORYNTH-META-1\n";
const ARTIFACT_MAGIC: &[u8] = b"ORYNTH-BLOB-1\n";
const ARTIFACT_MAGIC_V2: &[u8] = b"ORYNTH-BLOB-2\n";
const CHECKSUM_BYTES: usize = 32;
const FRAME_HEADER_BYTES: usize = 12;
const METADATA_FRAME_HEADER_BYTES: usize = 5;
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

type FileMetadata = (
    BTreeMap<BranchId, BranchMetadata>,
    BTreeMap<(RunId, Sequence), RuntimeSnapshot>,
);

pub struct FileEventStore {
    path: PathBuf,
    metadata_path: PathBuf,
    _lock_file: File,
    memory: InMemoryEventStore,
    branches: BTreeMap<BranchId, BranchMetadata>,
    snapshots: BTreeMap<(RunId, Sequence), RuntimeSnapshot>,
}

impl FileEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(storage_error)?;
        }

        let lock_path = lock_path(&path);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(storage_error)?;
        lock_file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                StoreError::Storage(format!("event store is already open: {}", path.display()))
            }
            std::fs::TryLockError::Error(error) => storage_error(error),
        })?;

        let mut bytes = if path.exists() {
            fs::read(&path).map_err(storage_error)?
        } else {
            Vec::new()
        };

        if bytes.is_empty() {
            write_new_file(&path, EVENT_MAGIC).map_err(storage_error)?;
            bytes.extend_from_slice(EVENT_MAGIC);
        }

        if !bytes.starts_with(EVENT_MAGIC) {
            return Err(StoreError::Corrupt(
                "event store header is invalid".to_string(),
            ));
        }

        let mut memory = InMemoryEventStore::new();
        let mut offset = EVENT_MAGIC.len();
        let mut needs_repair = false;

        while offset < bytes.len() {
            let frame_start = offset;
            if bytes.len() - offset < FRAME_HEADER_BYTES {
                needs_repair = true;
                offset = frame_start;
                break;
            }

            let sequence = read_u64(&bytes[offset..offset + 8]);
            let payload_len = read_u32(&bytes[offset + 8..offset + 12]) as usize;
            if payload_len > MAX_FRAME_BYTES {
                return Err(StoreError::Corrupt(format!(
                    "event frame is too large: {payload_len} bytes"
                )));
            }

            let frame_len = FRAME_HEADER_BYTES
                .checked_add(payload_len)
                .and_then(|length| length.checked_add(CHECKSUM_BYTES))
                .ok_or_else(|| StoreError::Corrupt("event frame length overflow".to_string()))?;
            let frame_end = frame_start
                .checked_add(frame_len)
                .ok_or_else(|| StoreError::Corrupt("event frame offset overflow".to_string()))?;

            if frame_end > bytes.len() {
                needs_repair = true;
                offset = frame_start;
                break;
            }

            let payload_start = frame_start + FRAME_HEADER_BYTES;
            let payload_end = payload_start + payload_len;
            let payload = &bytes[payload_start..payload_end];
            let stored_checksum = &bytes[payload_end..frame_end];
            let actual_checksum = DeterministicContentHasher.hash(payload).as_bytes();
            if stored_checksum != actual_checksum {
                return Err(StoreError::Corrupt(format!(
                    "checksum mismatch at byte offset {frame_start}"
                )));
            }

            let event = decode_event(payload).map_err(StoreError::Corrupt)?;
            let assigned = memory.append(event)?;
            if assigned != sequence {
                return Err(StoreError::Corrupt(format!(
                    "expected sequence {}, found {}",
                    assigned, sequence
                )));
            }
            offset = frame_end;
        }

        if needs_repair {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(storage_error)?;
            file.set_len(offset as u64).map_err(storage_error)?;
            file.sync_all().map_err(storage_error)?;
        }

        let metadata_path = metadata_path(&path);
        let (branches, snapshots) = load_metadata(&metadata_path, &memory)?;

        Ok(Self {
            path,
            metadata_path,
            _lock_file: lock_file,
            memory,
            branches,
            snapshots,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn all_events(&self) -> &[StoredEvent] {
        self.memory.all_events()
    }

    pub fn append_trace(&mut self, trace: &EventTrace) -> Result<Vec<Sequence>, StoreError> {
        self.append_batch(trace.events())
    }

    fn append_frames(&self, frames: &[(Sequence, Vec<u8>)]) -> Result<(), StoreError> {
        let mut bytes = Vec::new();
        for (sequence, payload) in frames {
            let payload_len = u32::try_from(payload.len())
                .map_err(|_| StoreError::Storage("event payload exceeds u32::MAX".to_string()))?;
            bytes.extend_from_slice(&sequence.to_le_bytes());
            bytes.extend_from_slice(&payload_len.to_le_bytes());
            bytes.extend_from_slice(payload);
            bytes.extend_from_slice(&DeterministicContentHasher.hash(payload).as_bytes());
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(storage_error)?;
        file.write_all(&bytes).map_err(storage_error)?;
        file.flush().map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
        Ok(())
    }
}

impl EventStore for FileEventStore {
    fn append(&mut self, event: Event) -> Result<Sequence, StoreError> {
        self.append_batch(std::slice::from_ref(&event))
            .map(|sequences| sequences[0])
    }

    fn append_batch(&mut self, events: &[Event]) -> Result<Vec<Sequence>, StoreError> {
        let mut new_ids = std::collections::BTreeSet::new();
        for event in events {
            if self.memory.contains_event_id(event.id) || !new_ids.insert(event.id) {
                return Err(StoreError::DuplicateEvent(event.id));
            }
        }
        let mut frames = Vec::with_capacity(events.len());
        let mut sequence = self.memory.next_sequence();
        for event in events {
            let payload = encode_event(event).map_err(StoreError::Corrupt)?;
            frames.push((sequence, payload));
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| StoreError::Storage("event sequence overflow".to_string()))?;
        }
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        self.append_frames(&frames)?;
        let sequences = self.memory.append_batch(events)?;
        debug_assert_eq!(sequences.len(), frames.len());
        Ok(sequences)
    }

    fn events(&self, run_id: RunId) -> Result<Vec<StoredEvent>, StoreError> {
        self.memory.events(run_id)
    }

    fn events_since(
        &self,
        run_id: RunId,
        sequence: Sequence,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        self.memory.events_since(run_id, sequence)
    }

    fn reconstruct(&self, run_id: RunId) -> Result<RuntimeState, StoreError> {
        self.memory.reconstruct(run_id)
    }

    fn snapshot(&self, run_id: RunId) -> Result<RuntimeSnapshot, StoreError> {
        self.memory.snapshot(run_id)
    }
}

impl BranchStore for FileEventStore {
    fn create_branch(
        &mut self,
        parent_run_id: RunId,
        fork_sequence: Sequence,
        replay_mode: ReplayMode,
        created_at_ms: u64,
    ) -> Result<BranchMetadata, StoreError> {
        validate_fork_sequence(self.memory.all_events(), parent_run_id, fork_sequence)?;
        let metadata = BranchMetadata {
            branch_id: BranchId::new(),
            parent_run_id,
            fork_sequence,
            replay_mode,
            created_at_ms,
        };
        let mut branches = self.branches.clone();
        branches.insert(metadata.branch_id, metadata.clone());
        persist_metadata(&self.metadata_path, &branches, &self.snapshots)?;
        self.branches = branches;
        Ok(metadata)
    }

    fn branch(&self, branch_id: BranchId) -> Result<Option<BranchMetadata>, StoreError> {
        Ok(self.branches.get(&branch_id).cloned())
    }

    fn branches_for_run(&self, parent_run_id: RunId) -> Result<Vec<BranchMetadata>, StoreError> {
        Ok(self
            .branches
            .values()
            .filter(|branch| branch.parent_run_id == parent_run_id)
            .cloned()
            .collect())
    }
}

impl SnapshotStore for FileEventStore {
    fn save_snapshot(&mut self, snapshot: &RuntimeSnapshot) -> Result<(), StoreError> {
        validate_snapshot(self.memory.all_events(), snapshot)?;
        let mut snapshots = self.snapshots.clone();
        if let Some(existing) = snapshots.get(&(snapshot.run_id, snapshot.at_sequence))
            && existing != snapshot
        {
            return Err(StoreError::InvalidTransition(format!(
                "snapshot {} at sequence {} is immutable",
                snapshot.run_id, snapshot.at_sequence
            )));
        }
        snapshots.insert((snapshot.run_id, snapshot.at_sequence), snapshot.clone());
        persist_metadata(&self.metadata_path, &self.branches, &snapshots)?;
        self.snapshots = snapshots;
        Ok(())
    }

    fn load_snapshot(
        &self,
        run_id: RunId,
        at_or_before: Option<Sequence>,
    ) -> Result<Option<RuntimeSnapshot>, StoreError> {
        Ok(self
            .snapshots
            .range((run_id, 0)..=(run_id, at_or_before.unwrap_or(Sequence::MAX)))
            .next_back()
            .map(|(_, snapshot)| snapshot.clone()))
    }
}

fn metadata_path(event_path: &Path) -> PathBuf {
    event_path.with_extension("meta")
}

fn lock_path(event_path: &Path) -> PathBuf {
    event_path.with_extension("lock")
}

fn load_metadata(path: &Path, events: &InMemoryEventStore) -> Result<FileMetadata, StoreError> {
    if !path.exists() {
        return Ok((BTreeMap::new(), BTreeMap::new()));
    }
    let bytes = fs::read(path).map_err(storage_error)?;
    if !bytes.starts_with(METADATA_MAGIC) {
        return Err(StoreError::Corrupt(
            "metadata store header is invalid".to_string(),
        ));
    }

    let mut branches = BTreeMap::new();
    let mut snapshots = BTreeMap::new();
    let mut offset = METADATA_MAGIC.len();
    let mut needs_repair = false;
    while offset < bytes.len() {
        let frame_start = offset;
        if bytes.len() - offset < METADATA_FRAME_HEADER_BYTES {
            needs_repair = true;
            break;
        }
        let kind = bytes[offset];
        let payload_len = read_u32(&bytes[offset + 1..offset + 5]) as usize;
        if payload_len > MAX_FRAME_BYTES {
            return Err(StoreError::Corrupt(format!(
                "metadata frame is too large: {payload_len} bytes"
            )));
        }
        let frame_len = METADATA_FRAME_HEADER_BYTES
            .checked_add(payload_len)
            .and_then(|length| length.checked_add(CHECKSUM_BYTES))
            .ok_or_else(|| StoreError::Corrupt("metadata frame length overflow".to_string()))?;
        let frame_end = frame_start
            .checked_add(frame_len)
            .ok_or_else(|| StoreError::Corrupt("metadata frame offset overflow".to_string()))?;
        if frame_end > bytes.len() {
            needs_repair = true;
            break;
        }
        let payload_start = frame_start + METADATA_FRAME_HEADER_BYTES;
        let payload_end = payload_start + payload_len;
        let payload = &bytes[payload_start..payload_end];
        let checksum = &bytes[payload_end..frame_end];
        if checksum != DeterministicContentHasher.hash(payload).as_bytes() {
            return Err(StoreError::Corrupt(format!(
                "metadata checksum mismatch at byte offset {frame_start}"
            )));
        }
        match kind {
            1 => {
                let branch = decode_branch_metadata(payload)?;
                if branches.insert(branch.branch_id, branch).is_some() {
                    return Err(StoreError::Corrupt(
                        "duplicate branch metadata record".to_string(),
                    ));
                }
            }
            2 => {
                let snapshot = decode_snapshot_metadata(payload)?;
                if snapshots
                    .insert((snapshot.run_id, snapshot.at_sequence), snapshot)
                    .is_some()
                {
                    return Err(StoreError::Corrupt(
                        "duplicate snapshot metadata record".to_string(),
                    ));
                }
            }
            other => {
                return Err(StoreError::Corrupt(format!(
                    "unknown metadata record kind {other}"
                )));
            }
        }
        offset = frame_end;
    }

    if needs_repair {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(storage_error)?;
        file.set_len(offset as u64).map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
    }

    for branch in branches.values() {
        validate_fork_sequence(
            events.all_events(),
            branch.parent_run_id,
            branch.fork_sequence,
        )?;
    }
    for snapshot in snapshots.values() {
        validate_snapshot(events.all_events(), snapshot)?;
    }
    Ok((branches, snapshots))
}

fn persist_metadata(
    path: &Path,
    branches: &BTreeMap<BranchId, BranchMetadata>,
    snapshots: &BTreeMap<(RunId, Sequence), RuntimeSnapshot>,
) -> Result<(), StoreError> {
    let mut bytes = Vec::from(METADATA_MAGIC);
    for branch in branches.values() {
        append_metadata_frame(&mut bytes, 1, &encode_branch_metadata(branch))?;
    }
    for snapshot in snapshots.values() {
        let payload = encode_snapshot_metadata(snapshot)?;
        append_metadata_frame(&mut bytes, 2, &payload)?;
    }

    let temporary = path.with_extension("meta.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(storage_error)?;
    file.write_all(&bytes).map_err(storage_error)?;
    file.flush().map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    if path.exists() {
        fs::remove_file(path).map_err(storage_error)?;
    }
    fs::rename(temporary, path).map_err(storage_error)?;
    Ok(())
}

fn append_metadata_frame(bytes: &mut Vec<u8>, kind: u8, payload: &[u8]) -> Result<(), StoreError> {
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| StoreError::Storage("metadata payload exceeds u32::MAX".to_string()))?;
    bytes.push(kind);
    bytes.extend_from_slice(&payload_len.to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&DeterministicContentHasher.hash(payload).as_bytes());
    Ok(())
}

fn encode_branch_metadata(branch: &BranchMetadata) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(&branch.branch_id.value().to_le_bytes());
    bytes.extend_from_slice(&branch.parent_run_id.value().to_le_bytes());
    bytes.extend_from_slice(&branch.fork_sequence.to_le_bytes());
    bytes.push(match branch.replay_mode {
        ReplayMode::Recorded => 0,
        ReplayMode::ReexecuteLive => 1,
        ReplayMode::ForkLive => 2,
    });
    bytes.extend_from_slice(&branch.created_at_ms.to_le_bytes());
    bytes
}

fn decode_branch_metadata(bytes: &[u8]) -> Result<BranchMetadata, StoreError> {
    if bytes.len() != 33 {
        return Err(StoreError::Corrupt(
            "branch metadata payload has invalid length".to_string(),
        ));
    }
    let branch_id = BranchId::from_u64(read_u64(&bytes[0..8]));
    let parent_run_id = RunId::from_u64(read_u64(&bytes[8..16]));
    let fork_sequence = read_u64(&bytes[16..24]);
    let replay_mode = match bytes[24] {
        0 => ReplayMode::Recorded,
        1 => ReplayMode::ReexecuteLive,
        2 => ReplayMode::ForkLive,
        tag => {
            return Err(StoreError::Corrupt(format!(
                "unknown replay mode tag {tag}"
            )));
        }
    };
    Ok(BranchMetadata {
        branch_id,
        parent_run_id,
        fork_sequence,
        replay_mode,
        created_at_ms: read_u64(&bytes[25..33]),
    })
}

fn encode_snapshot_metadata(snapshot: &RuntimeSnapshot) -> Result<Vec<u8>, StoreError> {
    let state = encode_snapshot_state(&snapshot.state).map_err(StoreError::Corrupt)?;
    let state_len = u32::try_from(state.len())
        .map_err(|_| StoreError::Storage("snapshot state exceeds u32::MAX".to_string()))?;
    let mut bytes = Vec::with_capacity(20 + state.len());
    bytes.extend_from_slice(&snapshot.run_id.value().to_le_bytes());
    bytes.extend_from_slice(&snapshot.at_sequence.to_le_bytes());
    bytes.extend_from_slice(&state_len.to_le_bytes());
    bytes.extend_from_slice(&state);
    Ok(bytes)
}

fn decode_snapshot_metadata(bytes: &[u8]) -> Result<RuntimeSnapshot, StoreError> {
    if bytes.len() < 20 {
        return Err(StoreError::Corrupt(
            "snapshot metadata payload is truncated".to_string(),
        ));
    }
    let run_id = RunId::from_u64(read_u64(&bytes[0..8]));
    let at_sequence = read_u64(&bytes[8..16]);
    let state_len = read_u32(&bytes[16..20]) as usize;
    let state_end = 20usize
        .checked_add(state_len)
        .ok_or_else(|| StoreError::Corrupt("snapshot state length overflow".to_string()))?;
    if state_end != bytes.len() {
        return Err(StoreError::Corrupt(
            "snapshot metadata payload has invalid length".to_string(),
        ));
    }
    let state = decode_snapshot_state(&bytes[20..state_end]).map_err(StoreError::Corrupt)?;
    Ok(RuntimeSnapshot {
        run_id,
        at_sequence,
        state,
    })
}

pub struct FileArtifactStore<H = DeterministicContentHasher> {
    root: PathBuf,
    hasher: H,
}

impl FileArtifactStore<DeterministicContentHasher> {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        Self::with_hasher(root, DeterministicContentHasher)
    }
}

impl<H> FileArtifactStore<H> {
    pub fn with_hasher(root: impl AsRef<Path>, hasher: H) -> Result<Self, ArtifactError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(artifact_storage_error)?;
        Ok(Self { root, hasher })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, content_hash: ContentHash) -> PathBuf {
        self.root.join(format!("{content_hash}.blob"))
    }
}

impl<H: ContentHasher> ArtifactStore for FileArtifactStore<H> {
    fn put_with_trust(
        &mut self,
        media_type: &str,
        bytes: Vec<u8>,
        trust: TrustOrigin,
    ) -> Result<ArtifactRef, ArtifactError> {
        if media_type.trim().is_empty() {
            return Err(ArtifactError::InvalidMediaType);
        }

        let content_hash = self.hasher.hash(&bytes);
        let reference = ArtifactRef {
            content_hash,
            size_bytes: bytes.len() as u64,
            media_type: media_type.to_string(),
            trust,
        };
        let path = self.path_for(content_hash);
        let lock_path = artifact_lock_path(&path);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(artifact_storage_error)?;
        lock_file.lock().map_err(artifact_storage_error)?;

        if path.exists() {
            let existing = decode_artifact(
                &fs::read(&path).map_err(artifact_storage_error)?,
                content_hash,
                &self.hasher,
            )?;
            if existing.bytes() != bytes {
                return Err(ArtifactError::HashCollision(content_hash));
            }
            if existing.reference.media_type != reference.media_type {
                return Err(ArtifactError::MetadataMismatch(content_hash));
            }
            let combined = existing.reference.trust.combine(reference.trust);
            if existing.reference.trust == combined {
                return Ok(existing.reference);
            }
            let updated = ArtifactRef {
                trust: combined,
                ..existing.reference
            };
            write_artifact_file(&path, &updated, &bytes)?;
            return Ok(updated);
        }

        write_artifact_file(&path, &reference, &bytes)?;
        Ok(reference)
    }

    fn get(&self, content_hash: ContentHash) -> Result<Option<StoredArtifact>, ArtifactError> {
        let path = self.path_for(content_hash);
        match fs::read(&path) {
            Ok(bytes) => decode_artifact(&bytes, content_hash, &self.hasher).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(artifact_storage_error(error)),
        }
    }
}

fn write_artifact_file(
    path: &Path,
    reference: &ArtifactRef,
    bytes: &[u8],
) -> Result<(), ArtifactError> {
    let encoded = encode_artifact(reference, bytes).map_err(ArtifactError::Corrupt)?;
    let temporary = artifact_temp_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(artifact_storage_error)?;
    file.write_all(&encoded).map_err(artifact_storage_error)?;
    file.flush().map_err(artifact_storage_error)?;
    file.sync_all().map_err(artifact_storage_error)?;
    fs::rename(&temporary, path).map_err(artifact_storage_error)?;
    Ok(())
}

fn write_new_file(path: &Path, magic: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    file.write_all(magic)?;
    file.flush()?;
    file.sync_all()
}

fn artifact_lock_path(path: &Path) -> PathBuf {
    path.with_extension("blob.lock")
}

fn artifact_temp_path(path: &Path) -> PathBuf {
    path.with_extension("blob.tmp")
}

fn storage_error(error: io::Error) -> StoreError {
    StoreError::Storage(error.to_string())
}

fn artifact_storage_error(error: io::Error) -> ArtifactError {
    ArtifactError::Storage(error.to_string())
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("caller validates u32 width"))
}

fn read_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("caller validates u64 width"))
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn array<const N: usize>(&mut self, value: &[u8; N]) {
        self.bytes.extend_from_slice(value);
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), String> {
        let length =
            u32::try_from(value.len()).map_err(|_| "value exceeds u32::MAX".to_string())?;
        self.u32(length);
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), String> {
        self.bytes(value.as_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "decode offset overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("unexpected end of encoded value".to_string());
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("width checked"),
        ))
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("width checked"),
        ))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("width checked"),
        ))
    }

    fn u128(&mut self) -> Result<u128, String> {
        Ok(u128::from_le_bytes(
            self.take(16)?.try_into().expect("width checked"),
        ))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        Ok(self.take(N)?.try_into().expect("width checked"))
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let length = self.u32()? as usize;
        if length > MAX_FRAME_BYTES {
            return Err("encoded value exceeds maximum frame size".to_string());
        }
        Ok(self.take(length)?.to_vec())
    }

    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.bytes()?).map_err(|_| "encoded string is not UTF-8".to_string())
    }

    fn finish(self) -> Result<(), String> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("encoded value contains trailing bytes".to_string())
        }
    }
}

pub(crate) fn encode_event(event: &Event) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new();
    writer.u64(event.id.value());
    writer.u64(event.run_id.value());
    writer.u128(event.occurred_at_ms);

    match &event.kind {
        EventKind::RunCreated { run_id } => {
            writer.u8(0);
            writer.u64(run_id.value());
        }
        EventKind::TaskCreated {
            task_id,
            run_id,
            title,
        } => {
            writer.u8(1);
            writer.u64(task_id.value());
            writer.u64(run_id.value());
            writer.string(title)?;
        }
        EventKind::AgentCreated { agent } => {
            writer.u8(2);
            encode_identity(&mut writer, agent)?;
        }
        EventKind::ModelRequested { agent_id, model } => {
            writer.u8(3);
            writer.u64(agent_id.value());
            encode_model(&mut writer, model)?;
        }
        EventKind::ModelChunkReceived {
            agent_id,
            chunk_index,
        } => {
            writer.u8(4);
            writer.u64(agent_id.value());
            writer.u32(*chunk_index);
        }
        EventKind::ModelCompleted { agent_id, usage } => {
            writer.u8(5);
            writer.u64(agent_id.value());
            writer.u64(usage.input_tokens);
            writer.u64(usage.output_tokens);
            match usage.cached_input_tokens {
                Some(cached_input_tokens) => {
                    writer.u8(1);
                    writer.u64(cached_input_tokens);
                }
                None => writer.u8(0),
            }
        }
        EventKind::ModelCancelled { agent_id } => {
            writer.u8(6);
            writer.u64(agent_id.value());
        }
        EventKind::ModelFailed { agent_id, message } => {
            writer.u8(7);
            writer.u64(agent_id.value());
            writer.string(message)?;
        }
        EventKind::RunCompleted { run_id } => {
            writer.u8(8);
            writer.u64(run_id.value());
        }
        EventKind::RunCancelled { run_id } => {
            writer.u8(9);
            writer.u64(run_id.value());
        }
        EventKind::RunFailed { run_id, message } => {
            writer.u8(10);
            writer.u64(run_id.value());
            writer.string(message)?;
        }
        EventKind::ArtifactCreated {
            content_hash,
            size_bytes,
            media_type,
            trust,
        } => {
            writer.u8(11);
            writer.array(content_hash);
            writer.u64(*size_bytes);
            writer.string(media_type)?;
            writer.u8(trust_tag(*trust));
        }
        EventKind::ContextTransition { version, payload } => {
            writer.u8(12);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::CacheObserved {
            provider,
            model,
            prefix_hash,
            estimated_prefix_tokens,
            cached_input_tokens,
        } => {
            writer.u8(13);
            writer.string(provider)?;
            writer.string(model)?;
            writer.array(prefix_hash);
            writer.u64(*estimated_prefix_tokens);
            writer.u64(*cached_input_tokens);
        }
        EventKind::AgentMessage { version, payload } => {
            writer.u8(14);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::AssumptionTransition { version, payload } => {
            writer.u8(15);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::SchedulerTransition { version, payload } => {
            writer.u8(16);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::AgentPaused { agent_id } => {
            writer.u8(17);
            writer.u64(agent_id.value());
        }
        EventKind::AgentResumed { agent_id } => {
            writer.u8(18);
            writer.u64(agent_id.value());
        }
        EventKind::CapabilityTransition { version, payload } => {
            writer.u8(19);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::ToolTransition { version, payload } => {
            writer.u8(20);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::FailureMemoryTransition { version, payload } => {
            writer.u8(21);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
        EventKind::SpecialistTransition { version, payload } => {
            writer.u8(22);
            writer.u16(*version);
            writer.bytes(payload)?;
        }
    }

    Ok(writer.finish())
}

fn encode_model(writer: &mut Writer, model: &ModelRef) -> Result<(), String> {
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

fn encode_identity(writer: &mut Writer, agent: &AgentIdentity) -> Result<(), String> {
    writer.u64(agent.id.value());
    writer.string(&agent.name)?;
    writer.string(&agent.mission)?;
    encode_model(writer, &agent.model)
}

pub(crate) fn decode_event(bytes: &[u8]) -> Result<Event, String> {
    let mut cursor = Cursor::new(bytes);
    let id = EventId::from_u64(cursor.u64()?);
    let run_id = RunId::from_u64(cursor.u64()?);
    let occurred_at_ms = cursor.u128()?;
    let kind = match cursor.u8()? {
        0 => EventKind::RunCreated {
            run_id: RunId::from_u64(cursor.u64()?),
        },
        1 => EventKind::TaskCreated {
            task_id: TaskId::from_u64(cursor.u64()?),
            run_id: RunId::from_u64(cursor.u64()?),
            title: cursor.string()?,
        },
        2 => EventKind::AgentCreated {
            agent: decode_identity(&mut cursor)?,
        },
        3 => EventKind::ModelRequested {
            agent_id: AgentId::from_u64(cursor.u64()?),
            model: decode_model(&mut cursor)?,
        },
        4 => EventKind::ModelChunkReceived {
            agent_id: AgentId::from_u64(cursor.u64()?),
            chunk_index: cursor.u32()?,
        },
        5 => {
            let agent_id = AgentId::from_u64(cursor.u64()?);
            let input_tokens = cursor.u64()?;
            let output_tokens = cursor.u64()?;
            let cached_input_tokens = if cursor.remaining() == 0 {
                None
            } else {
                match cursor.u8()? {
                    0 => None,
                    1 => Some(cursor.u64()?),
                    tag => return Err(format!("unknown cached usage tag {tag}")),
                }
            };
            EventKind::ModelCompleted {
                agent_id,
                usage: cached_input_tokens.map_or_else(
                    || Usage::new(input_tokens, output_tokens),
                    |cached| {
                        Usage::new(input_tokens, output_tokens).with_cached_input_tokens(cached)
                    },
                ),
            }
        }
        6 => EventKind::ModelCancelled {
            agent_id: AgentId::from_u64(cursor.u64()?),
        },
        7 => EventKind::ModelFailed {
            agent_id: AgentId::from_u64(cursor.u64()?),
            message: cursor.string()?,
        },
        8 => EventKind::RunCompleted {
            run_id: RunId::from_u64(cursor.u64()?),
        },
        9 => EventKind::RunCancelled {
            run_id: RunId::from_u64(cursor.u64()?),
        },
        10 => EventKind::RunFailed {
            run_id: RunId::from_u64(cursor.u64()?),
            message: cursor.string()?,
        },
        11 => {
            let content_hash = cursor.array()?;
            let size_bytes = cursor.u64()?;
            let media_type = cursor.string()?;
            let trust = if cursor.remaining() == 0 {
                TrustOrigin::Generated
            } else {
                decode_trust_tag(cursor.u8()?)?
            };
            EventKind::ArtifactCreated {
                content_hash,
                size_bytes,
                media_type,
                trust,
            }
        }
        12 => EventKind::ContextTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        13 => EventKind::CacheObserved {
            provider: cursor.string()?,
            model: cursor.string()?,
            prefix_hash: cursor.array()?,
            estimated_prefix_tokens: cursor.u64()?,
            cached_input_tokens: cursor.u64()?,
        },
        14 => EventKind::AgentMessage {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        15 => EventKind::AssumptionTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        16 => EventKind::SchedulerTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        17 => EventKind::AgentPaused {
            agent_id: AgentId::from_u64(cursor.u64()?),
        },
        18 => EventKind::AgentResumed {
            agent_id: AgentId::from_u64(cursor.u64()?),
        },
        19 => EventKind::CapabilityTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        20 => EventKind::ToolTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        21 => EventKind::FailureMemoryTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        22 => EventKind::SpecialistTransition {
            version: cursor.u16()?,
            payload: cursor.bytes()?,
        },
        tag => return Err(format!("unknown event kind tag {tag}")),
    };
    cursor.finish()?;
    Ok(Event {
        id,
        run_id,
        occurred_at_ms,
        kind,
    })
}

fn decode_model(cursor: &mut Cursor<'_>) -> Result<ModelRef, String> {
    let provider = cursor.string()?;
    let model = cursor.string()?;
    let class = match cursor.u8()? {
        0 => ModelClass::Local,
        1 => ModelClass::Cheap,
        2 => ModelClass::Strong,
        3 => ModelClass::Custom(cursor.string()?),
        tag => return Err(format!("unknown model class tag {tag}")),
    };
    Ok(ModelRef::new(provider, model, class))
}

fn decode_identity(cursor: &mut Cursor<'_>) -> Result<AgentIdentity, String> {
    Ok(AgentIdentity {
        id: AgentId::from_u64(cursor.u64()?),
        name: cursor.string()?,
        mission: cursor.string()?,
        model: decode_model(cursor)?,
    })
}

fn trust_tag(trust: TrustOrigin) -> u8 {
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

fn decode_trust_tag(tag: u8) -> Result<TrustOrigin, String> {
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
        tag => Err(format!("unknown trust origin tag {tag}")),
    }
}

fn encode_artifact(reference: &ArtifactRef, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new();
    writer.bytes(ARTIFACT_MAGIC_V2)?;
    writer.u64(reference.size_bytes);
    writer.string(&reference.media_type)?;
    writer.array(&reference.content_hash.as_bytes());
    writer.u8(trust_tag(reference.trust));
    writer.bytes(bytes)?;
    Ok(writer.finish())
}

fn decode_artifact<H: ContentHasher>(
    bytes: &[u8],
    expected_hash: ContentHash,
    hasher: &H,
) -> Result<StoredArtifact, ArtifactError> {
    let mut cursor = Cursor::new(bytes);
    let magic = cursor.bytes().map_err(ArtifactError::Corrupt)?;
    let has_trust = match magic.as_slice() {
        ARTIFACT_MAGIC => false,
        ARTIFACT_MAGIC_V2 => true,
        _ => {
            return Err(ArtifactError::Corrupt(
                "artifact header is invalid".to_string(),
            ));
        }
    };
    if magic != ARTIFACT_MAGIC && magic != ARTIFACT_MAGIC_V2 {
        return Err(ArtifactError::Corrupt(
            "artifact header is invalid".to_string(),
        ));
    }
    let size_bytes = cursor.u64().map_err(ArtifactError::Corrupt)?;
    let media_type = cursor.string().map_err(ArtifactError::Corrupt)?;
    let content_hash = ContentHash::from_digest(cursor.array().map_err(ArtifactError::Corrupt)?);
    let trust = if has_trust {
        decode_trust_tag(cursor.u8().map_err(ArtifactError::Corrupt)?)
            .map_err(ArtifactError::Corrupt)?
    } else {
        TrustOrigin::Generated
    };
    let payload = cursor.bytes().map_err(ArtifactError::Corrupt)?;
    cursor.finish().map_err(ArtifactError::Corrupt)?;

    if content_hash != expected_hash {
        return Err(ArtifactError::Corrupt(
            "artifact filename/hash mismatch".to_string(),
        ));
    }
    if size_bytes != payload.len() as u64 {
        return Err(ArtifactError::Corrupt("artifact size mismatch".to_string()));
    }
    if hasher.hash(&payload) != expected_hash {
        return Err(ArtifactError::Corrupt(
            "artifact content hash mismatch".to_string(),
        ));
    }

    Ok(StoredArtifact {
        reference: ArtifactRef {
            content_hash,
            size_bytes,
            media_type,
            trust,
        },
        bytes: payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_agent::AgentSession;
    use orynth_assumptions::{
        ASSUMPTION_SCHEMA_VERSION, Assumption, AssumptionTransition, decode_transition,
        encode_transition,
    };
    use orynth_ipc::{IPC_SCHEMA_VERSION, IpcEnvelope, IpcMessage, IpcProvenance};
    use orynth_kernel::{AgentIdentity, CancellationToken, ModelClass, ModelRef};
    use orynth_provider::MockProvider;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_path(label: &str) -> PathBuf {
        let number = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("orynth-{label}-{}-{number}", std::process::id()))
    }

    fn successful_trace() -> EventTrace {
        let model = ModelRef::new("mock", "durable", ModelClass::Cheap);
        let identity = AgentIdentity::new("durable-agent", "test persistence", model.clone());
        AgentSession::new(identity, MockProvider::new(model, "durable response"))
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
    fn event_store_survives_reopen() {
        let path = temp_path("events");
        let trace = successful_trace();
        let run_id = run_id(&trace);

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        let state = reopened
            .reconstruct(run_id)
            .expect("state should reconstruct");
        assert_eq!(state.status, RunStatus::Completed);
        assert_eq!(reopened.all_events().len(), trace.len());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn cached_usage_survives_filesystem_reopen() {
        let path = temp_path("cached-usage");
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

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("file store should reopen");
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("state should reconstruct")
                .agents
                .get(&identity.id)
                .expect("agent should reconstruct")
                .usage
                .cached_input_tokens,
            Some(12)
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn cache_observation_survives_filesystem_reopen() {
        let path = temp_path("cache-observation");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::CacheObserved {
                provider: "mock".to_owned(),
                model: "cached".to_owned(),
                prefix_hash: [11; 32],
                estimated_prefix_tokens: 24,
                cached_input_tokens: 18,
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::CacheObserved {
                provider,
                model,
                prefix_hash,
                estimated_prefix_tokens: 24,
                cached_input_tokens: 18,
            } if provider == "mock" && model == "cached" && prefix_hash == &[11; 32]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn failure_memory_transition_survives_filesystem_reopen() {
        let path = temp_path("failure-memory-transition");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::FailureMemoryTransition {
                version: 1,
                payload: vec![9, 8, 7],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::FailureMemoryTransition { version: 1, payload }
                if payload == &[9, 8, 7]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn specialist_transition_survives_filesystem_reopen() {
        let path = temp_path("specialist-transition");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::SpecialistTransition {
                version: 1,
                payload: vec![4, 3, 2],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::SpecialistTransition { version: 1, payload }
                if payload == &[4, 3, 2]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn typed_agent_message_survives_filesystem_reopen() {
        let path = temp_path("agent-message");
        let run_id = RunId::new();
        let envelope = IpcEnvelope::new(
            run_id,
            None,
            orynth_kernel::AgentId::new(),
            orynth_kernel::AgentId::new(),
            IpcMessage::Question {
                subject: "schema.users.id".to_owned(),
                why: "authentication needs the canonical identifier".to_owned(),
            },
        )
        .with_provenance(IpcProvenance::Runtime);
        let payload = envelope.encode().expect("message should encode");
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::AgentMessage {
                version: IPC_SCHEMA_VERSION,
                payload,
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        let events = reopened.events(run_id).expect("events should read");
        let EventKind::AgentMessage { version, payload } = &events[1].event.kind else {
            panic!("expected an agent message event");
        };
        assert_eq!(*version, IPC_SCHEMA_VERSION);
        assert_eq!(
            IpcEnvelope::decode(*version, payload).expect("message should decode"),
            envelope
        );
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn assumption_transition_survives_filesystem_reopen() {
        let path = temp_path("assumption-transition");
        let run_id = RunId::new();
        let assumption = Assumption::new(
            run_id,
            orynth_kernel::AgentId::new(),
            "schema.users.id",
            "UUID",
            "users.id is UUID",
        );
        let payload = encode_transition(&AssumptionTransition::Created {
            assumption: assumption.clone(),
        })
        .expect("assumption should encode");
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::AssumptionTransition {
                version: ASSUMPTION_SCHEMA_VERSION,
                payload,
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
        }

        let reopened = FileEventStore::open(&path).expect("store should reopen");
        let events = reopened.events(run_id).expect("events should read");
        let EventKind::AssumptionTransition { version, payload } = &events[1].event.kind else {
            panic!("expected an assumption transition event");
        };
        assert_eq!(*version, ASSUMPTION_SCHEMA_VERSION);
        assert_eq!(
            decode_transition(*version, payload).expect("assumption should decode"),
            AssumptionTransition::Created { assumption }
        );
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn opaque_context_transition_survives_filesystem_reopen() {
        let path = temp_path("context-events");
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::ContextTransition {
                version: 1,
                payload: vec![1, 2, 3, 4],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store
                .append_trace(&trace)
                .expect("context event should persist");
        }

        let reopened = FileEventStore::open(&path).expect("file store should reopen");
        assert!(matches!(
            &reopened.events(run_id).expect("events should read")[1].event.kind,
            EventKind::ContextTransition { version: 1, payload } if payload == &[1, 2, 3, 4]
        ));
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("runtime state should reconstruct")
                .status,
            RunStatus::Completed
        );

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn batch_append_is_atomic_on_duplicate_ids() {
        let path = temp_path("batch-atomic");
        let trace = successful_trace();
        let event = trace.events()[0].clone();
        let mut store = FileEventStore::open(&path).expect("file store should open");

        assert!(matches!(
            store.append_batch(&[event.clone(), event]),
            Err(StoreError::DuplicateEvent(_))
        ));
        assert!(store.all_events().is_empty());

        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(metadata_path(&path));
    }

    #[test]
    fn metadata_store_persists_branches_and_snapshots() {
        let path = temp_path("metadata");
        let metadata = metadata_path(&path);
        let trace = successful_trace();
        let run_id = run_id(&trace);
        let branch;
        let snapshot;
        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
            branch = store
                .create_branch(run_id, 2, ReplayMode::ForkLive, 4321)
                .expect("branch should persist");
            snapshot = store.snapshot(run_id).expect("snapshot should compute");
            store
                .save_snapshot(&snapshot)
                .expect("snapshot should persist");
        }

        let reopened = FileEventStore::open(&path).expect("metadata should reopen");
        assert_eq!(
            reopened.branch(branch.branch_id).expect("branch lookup"),
            Some(branch)
        );
        assert_eq!(
            reopened
                .load_snapshot(run_id, None)
                .expect("snapshot lookup"),
            Some(snapshot)
        );

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(metadata);
    }

    #[test]
    fn fork_materialization_survives_filesystem_reopen() {
        let path = temp_path("fork");
        let metadata = metadata_path(&path);
        let trace = successful_trace();
        let parent_run_id = run_id(&trace);
        let child_run_id;
        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
            let branch = store
                .create_branch(parent_run_id, 2, ReplayMode::ForkLive, 8765)
                .expect("branch should persist");
            child_run_id = RunId::new();
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
        }

        let reopened = FileEventStore::open(&path).expect("file store should reopen");
        assert_eq!(
            reopened
                .reconstruct(child_run_id)
                .expect("child should reconstruct after reopen")
                .status,
            RunStatus::Active
        );
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(metadata);
    }

    #[test]
    fn metadata_torn_tail_is_repaired_and_checksum_corruption_is_rejected() {
        let path = temp_path("metadata-recovery");
        let metadata = metadata_path(&path);
        let trace = successful_trace();
        let run_id = run_id(&trace);
        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store.append_trace(&trace).expect("trace should persist");
            store
                .create_branch(run_id, 2, ReplayMode::Recorded, 5678)
                .expect("branch should persist");
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(&metadata)
            .expect("metadata should open");
        file.write_all(&[1, 2, 3]).expect("partial metadata frame");
        file.sync_all().expect("partial metadata should flush");

        let reopened = FileEventStore::open(&path).expect("metadata tail should repair");
        assert_eq!(
            reopened
                .branches_for_run(run_id)
                .expect("branch list")
                .len(),
            1
        );
        drop(reopened);

        let mut bytes = fs::read(&metadata).expect("metadata should read");
        bytes[METADATA_MAGIC.len() + METADATA_FRAME_HEADER_BYTES] ^= 0xff;
        fs::write(&metadata, bytes).expect("metadata corruption should persist");
        assert!(matches!(
            FileEventStore::open(&path),
            Err(StoreError::Corrupt(message)) if message.contains("metadata checksum")
        ));

        let _ = fs::remove_file(path);
        let _ = fs::remove_file(metadata);
    }

    #[test]
    fn incomplete_final_frame_is_repaired_on_open() {
        let path = temp_path("torn");
        let trace = successful_trace();
        let run_id = run_id(&trace);

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store
                .append(trace.events()[0].clone())
                .expect("first event should persist");
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("file should open");
        file.write_all(&[1, 2, 3, 4]).expect("partial frame");
        file.sync_all().expect("partial frame should flush");

        let reopened = FileEventStore::open(&path).expect("torn tail should be repaired");
        assert_eq!(reopened.all_events().len(), 1);
        assert_eq!(
            reopened
                .events(run_id)
                .expect("events should be readable")
                .len(),
            1
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn interrupted_batch_preserves_complete_prefix_and_resumes_sequences() {
        let path = temp_path("interrupted-batch");
        let trace = successful_trace();
        let run_id = run_id(&trace);

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store
                .append(trace.events()[0].clone())
                .expect("first event should persist");
        }

        let payload = encode_event(&trace.events()[1]).expect("event should encode");
        let mut partial_frame = Vec::new();
        partial_frame.extend_from_slice(&2_u64.to_le_bytes());
        partial_frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        partial_frame.extend_from_slice(&payload);
        partial_frame.extend_from_slice(&DeterministicContentHasher.hash(&payload).as_bytes());
        partial_frame.truncate(FRAME_HEADER_BYTES + payload.len() / 2);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("event file should open");
        file.write_all(&partial_frame)
            .expect("partial batch frame should write");
        file.sync_all().expect("partial batch frame should flush");

        let mut reopened = FileEventStore::open(&path).expect("prefix should recover");
        assert_eq!(reopened.all_events().len(), 1);
        let sequences = reopened
            .append_batch(&trace.events()[1..])
            .expect("remaining events should resume after recovery");
        assert_eq!(sequences.first(), Some(&2));
        assert_eq!(sequences.len(), trace.len() - 1);
        assert_eq!(
            reopened
                .reconstruct(run_id)
                .expect("recovered run should reconstruct")
                .status,
            RunStatus::Completed
        );

        drop(reopened);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn filesystem_store_enforces_exclusive_open_and_releases_lock() {
        let path = temp_path("lock");
        let store = FileEventStore::open(&path).expect("file store should open");
        assert!(matches!(
            FileEventStore::open(&path),
            Err(StoreError::Storage(message)) if message.contains("already open")
        ));
        drop(store);

        let reopened = FileEventStore::open(&path).expect("lock should release on drop");
        drop(reopened);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(lock_path(&path));
    }

    #[test]
    fn checksum_corruption_is_rejected() {
        let path = temp_path("corrupt");
        let trace = successful_trace();

        {
            let mut store = FileEventStore::open(&path).expect("file store should open");
            store
                .append(trace.events()[0].clone())
                .expect("event should persist");
        }
        let mut bytes = fs::read(&path).expect("file should read");
        let payload_offset = EVENT_MAGIC.len() + FRAME_HEADER_BYTES;
        bytes[payload_offset] ^= 0xff;
        fs::write(&path, bytes).expect("corruption should persist");

        assert!(matches!(
            FileEventStore::open(&path),
            Err(StoreError::Corrupt(message)) if message.contains("checksum")
        ));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn artifact_store_survives_reopen_and_validates_payload() {
        let root = temp_path("artifacts");
        let reference;
        {
            let mut store = FileArtifactStore::open(&root).expect("artifact store should open");
            reference = store
                .put_with_trust(
                    "text/plain",
                    b"durable artifact".to_vec(),
                    TrustOrigin::McpResult,
                )
                .expect("artifact should persist");
        }

        let reopened = FileArtifactStore::open(&root).expect("artifact store should reopen");
        let stored = reopened
            .get(reference.content_hash)
            .expect("artifact lookup should succeed")
            .expect("artifact should exist");
        assert_eq!(stored.reference, reference);
        assert_eq!(stored.reference.trust, TrustOrigin::McpResult);
        assert_eq!(stored.bytes(), b"durable artifact");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn artifact_put_recovers_orphaned_temp_and_serializes_same_hash_writers() {
        let root = temp_path("artifact-concurrent");
        let bytes = b"concurrent durable artifact".to_vec();
        let content_hash = DeterministicContentHasher.hash(&bytes);
        let path = root.join(format!("{content_hash}.blob"));
        fs::create_dir_all(&root).expect("artifact root should exist");
        fs::write(artifact_temp_path(&path), b"interrupted payload")
            .expect("orphaned temp should persist");

        let first_root = root.clone();
        let first_bytes = bytes.clone();
        let first = std::thread::spawn(move || {
            let mut store = FileArtifactStore::open(first_root).expect("first store should open");
            store
                .put("application/octet-stream", first_bytes)
                .expect("first artifact write should succeed")
        });
        let second_root = root.clone();
        let second_bytes = bytes.clone();
        let second = std::thread::spawn(move || {
            let mut store = FileArtifactStore::open(second_root).expect("second store should open");
            store
                .put("application/octet-stream", second_bytes)
                .expect("second artifact write should succeed")
        });

        let first_reference = first.join().expect("first writer should finish");
        let second_reference = second.join().expect("second writer should finish");
        assert_eq!(first_reference, second_reference);

        let reopened = FileArtifactStore::open(&root).expect("artifact store should reopen");
        let stored = reopened
            .get(content_hash)
            .expect("artifact lookup should succeed")
            .expect("artifact should exist");
        assert_eq!(stored.bytes(), bytes);
        assert!(!artifact_temp_path(&path).exists());

        let _ = fs::remove_dir_all(root);
    }
}
