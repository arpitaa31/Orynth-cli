use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orynth_kernel::{
    AgentId, AgentIdentity, BranchId, Event, EventId, EventKind, EventTrace, ModelClass, ModelRef,
    RunId, TaskId, TrustOrigin, Usage,
};

pub type Sequence = u64;

/// Inline artifact payloads are deliberately bounded. Larger immutable data
/// belongs in a future external content-addressed blob adapter rather than in
/// the event-store hot path or an inline SQLite row.
pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayMode {
    Recorded,
    ReexecuteLive,
    ForkLive,
}

impl ReplayMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::ReexecuteLive => "reexecute_live",
            Self::ForkLive => "fork_live",
        }
    }

    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "recorded" => Ok(Self::Recorded),
            "reexecute_live" => Ok(Self::ReexecuteLive),
            "fork_live" => Ok(Self::ForkLive),
            other => Err(StoreError::Corrupt(format!(
                "unknown branch replay mode {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchMetadata {
    pub branch_id: BranchId,
    pub parent_run_id: RunId,
    pub fork_sequence: Sequence,
    pub replay_mode: ReplayMode,
    pub created_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

pub trait ContentHasher {
    fn hash(&self, bytes: &[u8]) -> ContentHash;
}

/// A stable, deterministic identity hasher for the dependency-free in-memory backend.
/// It is not cryptographic and must not be used as an integrity or trust boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicContentHasher;

impl ContentHasher for DeterministicContentHasher {
    fn hash(&self, bytes: &[u8]) -> ContentHash {
        let mut lanes = [
            0xcbf29ce484222325_u64,
            0x84222325cbf29ce4_u64,
            0x9e3779b185ebca87_u64,
            0xd6e8feb86659fd93_u64,
        ];
        let multipliers = [
            0x100000001b3_u64,
            0x100000001c9_u64,
            0x100000001d7_u64,
            0x100000001f1_u64,
        ];

        for (index, byte) in bytes.iter().copied().enumerate() {
            for (lane, value) in lanes.iter_mut().enumerate() {
                *value ^=
                    u64::from(byte).wrapping_add((index as u64).rotate_left((lane * 7) as u32));
                *value = value.wrapping_mul(multipliers[lane]);
                *value ^= *value >> 29;
            }
        }

        let length = bytes.len() as u64;
        for (lane, value) in lanes.iter_mut().enumerate() {
            *value ^= length.rotate_left((lane * 11) as u32);
            *value = value.wrapping_mul(multipliers[lane]);
        }

        let mut digest = [0_u8; 32];
        for (lane, value) in lanes.into_iter().enumerate() {
            digest[lane * 8..lane * 8 + 8].copy_from_slice(&value.to_be_bytes());
        }
        ContentHash::from_digest(digest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRef {
    pub content_hash: ContentHash,
    pub size_bytes: u64,
    pub media_type: String,
    pub trust: TrustOrigin,
}

impl ArtifactRef {
    pub fn created_event(&self, run_id: RunId) -> Event {
        Event::new(
            run_id,
            EventKind::ArtifactCreated {
                content_hash: self.content_hash.as_bytes(),
                size_bytes: self.size_bytes,
                media_type: self.media_type.clone(),
                trust: self.trust,
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredArtifact {
    pub reference: ArtifactRef,
    bytes: Vec<u8>,
}

impl StoredArtifact {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtifactError {
    InvalidMediaType,
    TooLarge { actual: usize, maximum: usize },
    HashCollision(ContentHash),
    MetadataMismatch(ContentHash),
    Storage(String),
    Corrupt(String),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMediaType => formatter.write_str("artifact media type must not be empty"),
            Self::TooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "artifact is too large: {actual} bytes (maximum {maximum})"
                )
            }
            Self::HashCollision(hash) => write!(formatter, "content hash collision for {hash}"),
            Self::MetadataMismatch(hash) => {
                write!(
                    formatter,
                    "artifact metadata mismatch for content hash {hash}"
                )
            }
            Self::Storage(message) => write!(formatter, "artifact storage error: {message}"),
            Self::Corrupt(message) => write!(formatter, "corrupt artifact: {message}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

pub trait ArtifactStore {
    fn put(&mut self, media_type: &str, bytes: Vec<u8>) -> Result<ArtifactRef, ArtifactError> {
        self.put_with_trust(media_type, bytes, TrustOrigin::Generated)
    }

    fn put_with_trust(
        &mut self,
        media_type: &str,
        bytes: Vec<u8>,
        trust: TrustOrigin,
    ) -> Result<ArtifactRef, ArtifactError>;

    fn get(&self, content_hash: ContentHash) -> Result<Option<StoredArtifact>, ArtifactError>;
}

#[derive(Clone, Debug)]
pub struct InMemoryArtifactStore<H = DeterministicContentHasher> {
    hasher: H,
    artifacts: BTreeMap<ContentHash, StoredArtifact>,
}

impl InMemoryArtifactStore<DeterministicContentHasher> {
    pub fn new() -> Self {
        Self::with_hasher(DeterministicContentHasher)
    }
}

impl Default for InMemoryArtifactStore<DeterministicContentHasher> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> InMemoryArtifactStore<H> {
    pub fn with_hasher(hasher: H) -> Self {
        Self {
            hasher,
            artifacts: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.artifacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }
}

impl<H: ContentHasher> ArtifactStore for InMemoryArtifactStore<H> {
    fn put_with_trust(
        &mut self,
        media_type: &str,
        bytes: Vec<u8>,
        trust: TrustOrigin,
    ) -> Result<ArtifactRef, ArtifactError> {
        if media_type.trim().is_empty() {
            return Err(ArtifactError::InvalidMediaType);
        }
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::TooLarge {
                actual: bytes.len(),
                maximum: MAX_ARTIFACT_BYTES,
            });
        }

        let content_hash = self.hasher.hash(&bytes);
        let reference = ArtifactRef {
            content_hash,
            size_bytes: bytes.len() as u64,
            media_type: media_type.to_string(),
            trust,
        };

        if let Some(existing) = self.artifacts.get(&content_hash) {
            if existing.bytes != bytes {
                return Err(ArtifactError::HashCollision(content_hash));
            }
            if existing.reference.media_type != reference.media_type {
                return Err(ArtifactError::MetadataMismatch(content_hash));
            }
            let combined = existing.reference.trust.combine(reference.trust);
            let mut result = existing.reference.clone();
            result.trust = combined;
            if existing.reference.trust != combined
                && let Some(existing) = self.artifacts.get_mut(&content_hash)
            {
                existing.reference.trust = combined;
            }
            return Ok(result);
        }

        self.artifacts.insert(
            content_hash,
            StoredArtifact {
                reference: reference.clone(),
                bytes,
            },
        );
        Ok(reference)
    }

    fn get(&self, content_hash: ContentHash) -> Result<Option<StoredArtifact>, ArtifactError> {
        Ok(self.artifacts.get(&content_hash).cloned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredEvent {
    pub sequence: Sequence,
    pub event: Event,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    DuplicateEvent(EventId),
    UnknownBranch(BranchId),
    UnknownRun(RunId),
    UnknownAgent(AgentId),
    InvalidTransition(String),
    Storage(String),
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateEvent(id) => write!(formatter, "event {id} was already stored"),
            Self::UnknownBranch(id) => write!(formatter, "branch {id} is not present"),
            Self::UnknownRun(id) => write!(formatter, "run {id} is not present"),
            Self::UnknownAgent(id) => write!(formatter, "agent {id} is not present"),
            Self::InvalidTransition(message) => {
                write!(formatter, "invalid event transition: {message}")
            }
            Self::Storage(message) => write!(formatter, "event storage error: {message}"),
            Self::Corrupt(message) => write!(formatter, "corrupt event store: {message}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub trait EventStore {
    fn append(&mut self, event: Event) -> Result<Sequence, StoreError>;

    fn append_batch(&mut self, events: &[Event]) -> Result<Vec<Sequence>, StoreError>;

    fn events(&self, run_id: RunId) -> Result<Vec<StoredEvent>, StoreError>;

    fn events_since(
        &self,
        run_id: RunId,
        sequence: Sequence,
    ) -> Result<Vec<StoredEvent>, StoreError>;

    fn reconstruct(&self, run_id: RunId) -> Result<RuntimeState, StoreError>;

    fn snapshot(&self, run_id: RunId) -> Result<RuntimeSnapshot, StoreError>;
}

pub trait BranchStore {
    fn create_branch(
        &mut self,
        parent_run_id: RunId,
        fork_sequence: Sequence,
        replay_mode: ReplayMode,
        created_at_ms: u64,
    ) -> Result<BranchMetadata, StoreError>;

    fn branch(&self, branch_id: BranchId) -> Result<Option<BranchMetadata>, StoreError>;

    fn branches_for_run(&self, parent_run_id: RunId) -> Result<Vec<BranchMetadata>, StoreError>;
}

pub trait SnapshotStore {
    fn save_snapshot(&mut self, snapshot: &RuntimeSnapshot) -> Result<(), StoreError>;

    fn load_snapshot(
        &self,
        run_id: RunId,
        at_or_before: Option<Sequence>,
    ) -> Result<Option<RuntimeSnapshot>, StoreError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForkedRun {
    pub branch_id: BranchId,
    pub parent_run_id: RunId,
    pub child_run_id: RunId,
    pub fork_sequence: Sequence,
    pub replay_mode: ReplayMode,
    pub copied_event_count: u64,
    pub child_sequences: Vec<Sequence>,
    pub child_prefix: EventTrace,
}

pub trait ForkStore {
    fn materialize_fork(
        &mut self,
        branch_id: BranchId,
        child_run_id: RunId,
    ) -> Result<ForkedRun, StoreError>;
}

pub fn persist_fork_continuation<S: EventStore>(
    store: &mut S,
    fork: &ForkedRun,
    execution_trace: &EventTrace,
) -> Result<Vec<Sequence>, StoreError> {
    let prefix = fork.child_prefix.events();
    let execution_events = execution_trace.events();
    if execution_events.len() < prefix.len() || execution_events[..prefix.len()] != prefix[..] {
        return Err(StoreError::InvalidTransition(
            "fork continuation does not preserve the materialized child prefix".to_string(),
        ));
    }
    let persisted_prefix = store.events(fork.child_run_id)?;
    if persisted_prefix.len() < prefix.len()
        || persisted_prefix
            .iter()
            .take(prefix.len())
            .map(|stored| &stored.event)
            .ne(prefix.iter())
    {
        return Err(StoreError::InvalidTransition(
            "fork child prefix is not present in the target store".to_string(),
        ));
    }
    let continuation = &execution_events[prefix.len()..];
    if continuation.is_empty() {
        return Err(StoreError::InvalidTransition(
            "fork continuation contains no new events".to_string(),
        ));
    }
    if continuation
        .iter()
        .any(|event| event.run_id != fork.child_run_id)
    {
        return Err(StoreError::InvalidTransition(
            "fork continuation contains an event from another run".to_string(),
        ));
    }

    let mut validation = InMemoryEventStore::new();
    validation.append_batch(prefix)?;
    validation.append_batch(continuation)?;
    validation.reconstruct(fork.child_run_id)?;

    store.append_batch(continuation)
}

impl<T> ForkStore for T
where
    T: EventStore + BranchStore,
{
    fn materialize_fork(
        &mut self,
        branch_id: BranchId,
        child_run_id: RunId,
    ) -> Result<ForkedRun, StoreError> {
        if !self.events(child_run_id)?.is_empty() {
            return Err(StoreError::InvalidTransition(format!(
                "child run {child_run_id} already has events"
            )));
        }

        let branch = self
            .branch(branch_id)?
            .ok_or(StoreError::UnknownBranch(branch_id))?;
        if branch.parent_run_id == child_run_id {
            return Err(StoreError::InvalidTransition(
                "child run must differ from parent run".to_string(),
            ));
        }

        let parent_events = self.events(branch.parent_run_id)?;
        validate_fork_sequence(&parent_events, branch.parent_run_id, branch.fork_sequence)?;
        let prefix: Vec<Event> = parent_events
            .into_iter()
            .filter(|stored| stored.sequence <= branch.fork_sequence)
            .map(|stored| remap_event_for_run(&stored.event, child_run_id))
            .collect();
        if matches!(
            branch.replay_mode,
            ReplayMode::ForkLive | ReplayMode::ReexecuteLive
        ) && prefix.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::RunCompleted { .. }
                    | EventKind::RunCancelled { .. }
                    | EventKind::RunFailed { .. }
            )
        }) {
            return Err(StoreError::InvalidTransition(
                "live fork cannot begin after a terminal parent event".to_string(),
            ));
        }

        let mut child_prefix = EventTrace::default();
        for event in &prefix {
            child_prefix.record(event.clone());
        }
        let child_sequences = self.append_batch(&prefix)?;

        Ok(ForkedRun {
            branch_id,
            parent_run_id: branch.parent_run_id,
            child_run_id,
            fork_sequence: branch.fork_sequence,
            replay_mode: branch.replay_mode,
            copied_event_count: child_sequences.len() as u64,
            child_sequences,
            child_prefix,
        })
    }
}

fn remap_event_for_run(event: &Event, child_run_id: RunId) -> Event {
    let kind = match &event.kind {
        EventKind::RunCreated { .. } => EventKind::RunCreated {
            run_id: child_run_id,
        },
        EventKind::TaskCreated { task_id, title, .. } => EventKind::TaskCreated {
            task_id: *task_id,
            run_id: child_run_id,
            title: title.clone(),
        },
        EventKind::AgentCreated { agent } => EventKind::AgentCreated {
            agent: agent.clone(),
        },
        EventKind::ModelRequested { agent_id, model } => EventKind::ModelRequested {
            agent_id: *agent_id,
            model: model.clone(),
        },
        EventKind::ModelChunkReceived {
            agent_id,
            chunk_index,
        } => EventKind::ModelChunkReceived {
            agent_id: *agent_id,
            chunk_index: *chunk_index,
        },
        EventKind::ModelCompleted { agent_id, usage } => EventKind::ModelCompleted {
            agent_id: *agent_id,
            usage: *usage,
        },
        EventKind::ModelCancelled { agent_id } => EventKind::ModelCancelled {
            agent_id: *agent_id,
        },
        EventKind::ModelFailed { agent_id, message } => EventKind::ModelFailed {
            agent_id: *agent_id,
            message: message.clone(),
        },
        EventKind::RunCompleted { .. } => EventKind::RunCompleted {
            run_id: child_run_id,
        },
        EventKind::RunCancelled { .. } => EventKind::RunCancelled {
            run_id: child_run_id,
        },
        EventKind::RunFailed { message, .. } => EventKind::RunFailed {
            run_id: child_run_id,
            message: message.clone(),
        },
        EventKind::ArtifactCreated {
            content_hash,
            size_bytes,
            media_type,
            trust,
        } => EventKind::ArtifactCreated {
            content_hash: *content_hash,
            size_bytes: *size_bytes,
            media_type: media_type.clone(),
            trust: *trust,
        },
        EventKind::ContextTransition { version, payload } => EventKind::ContextTransition {
            version: *version,
            payload: payload.clone(),
        },
        EventKind::CacheObserved {
            provider,
            model,
            prefix_hash,
            estimated_prefix_tokens,
            cached_input_tokens,
        } => EventKind::CacheObserved {
            provider: provider.clone(),
            model: model.clone(),
            prefix_hash: *prefix_hash,
            estimated_prefix_tokens: *estimated_prefix_tokens,
            cached_input_tokens: *cached_input_tokens,
        },
        EventKind::AgentMessage { version, payload } => {
            let remapped = orynth_ipc::IpcEnvelope::decode(*version, payload)
                .map(|envelope| envelope.with_run_id(child_run_id).encode())
                .ok()
                .and_then(Result::ok);
            let (version, payload) = remapped
                .map(|payload| (orynth_ipc::IPC_SCHEMA_VERSION, payload))
                .unwrap_or((*version, payload.clone()));
            EventKind::AgentMessage { version, payload }
        }
        EventKind::AssumptionTransition { version, payload } => {
            let remapped = orynth_assumptions::decode_transition(*version, payload)
                .map(|transition| {
                    orynth_assumptions::encode_transition(&transition.with_run_id(child_run_id))
                })
                .ok()
                .and_then(Result::ok);
            let (version, payload) = remapped
                .map(|payload| (orynth_assumptions::ASSUMPTION_SCHEMA_VERSION, payload))
                .unwrap_or((*version, payload.clone()));
            EventKind::AssumptionTransition { version, payload }
        }
        EventKind::SchedulerTransition { version, payload } => EventKind::SchedulerTransition {
            version: *version,
            payload: payload.clone(),
        },
        EventKind::AgentPaused { agent_id } => EventKind::AgentPaused {
            agent_id: *agent_id,
        },
        EventKind::AgentResumed { agent_id } => EventKind::AgentResumed {
            agent_id: *agent_id,
        },
        EventKind::CapabilityTransition { version, payload } => EventKind::CapabilityTransition {
            version: *version,
            payload: payload.clone(),
        },
        EventKind::ToolTransition { version, payload } => {
            let remapped = orynth_tool_runtime::decode_transition(*version, payload)
                .map(|transition| {
                    orynth_tool_runtime::encode_transition(&transition.with_run_id(child_run_id))
                })
                .ok()
                .and_then(Result::ok);
            let (version, payload) = remapped
                .map(|payload| (orynth_tool_runtime::TOOL_SCHEMA_VERSION, payload))
                .unwrap_or((*version, payload.clone()));
            EventKind::ToolTransition { version, payload }
        }
        EventKind::FailureMemoryTransition { version, payload } => {
            EventKind::FailureMemoryTransition {
                version: *version,
                payload: payload.clone(),
            }
        }
        EventKind::SpecialistTransition { version, payload } => EventKind::SpecialistTransition {
            version: *version,
            payload: payload.clone(),
        },
    };
    Event {
        id: EventId::new(),
        run_id: child_run_id,
        occurred_at_ms: event.occurred_at_ms,
        kind,
    }
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryEventStore {
    next_sequence: Sequence,
    events: Vec<StoredEvent>,
    event_ids: BTreeSet<EventId>,
    branches: BTreeMap<BranchId, BranchMetadata>,
    snapshots: BTreeMap<(RunId, Sequence), RuntimeSnapshot>,
}

impl InMemoryEventStore {
    pub fn new() -> Self {
        Self {
            next_sequence: 1,
            events: Vec::new(),
            event_ids: BTreeSet::new(),
            branches: BTreeMap::new(),
            snapshots: BTreeMap::new(),
        }
    }

    pub fn append_trace(&mut self, trace: &EventTrace) -> Result<Vec<Sequence>, StoreError> {
        self.append_batch(trace.events())
    }

    pub fn all_events(&self) -> &[StoredEvent] {
        &self.events
    }

    pub(crate) fn contains_event_id(&self, event_id: EventId) -> bool {
        self.event_ids.contains(&event_id)
    }

    pub(crate) fn next_sequence(&self) -> Sequence {
        self.next_sequence
    }

    pub fn snapshot(&self, run_id: RunId) -> Result<RuntimeSnapshot, StoreError> {
        <Self as EventStore>::snapshot(self, run_id)
    }

    pub fn recorded_replay(&self, run_id: RunId) -> Result<RecordedReplay, StoreError> {
        Ok(RecordedReplay {
            events: self.events(run_id)?,
            state: self.reconstruct(run_id)?,
        })
    }
}

mod durable;
mod sqlite;

pub use durable::{FileArtifactStore, FileEventStore};
pub use sqlite::{SqliteArtifactStore, SqliteEventStore};

impl EventStore for InMemoryEventStore {
    fn append(&mut self, event: Event) -> Result<Sequence, StoreError> {
        if !self.event_ids.insert(event.id) {
            return Err(StoreError::DuplicateEvent(event.id));
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push(StoredEvent { sequence, event });
        Ok(sequence)
    }

    fn append_batch(&mut self, events: &[Event]) -> Result<Vec<Sequence>, StoreError> {
        let mut staged = self.clone();
        let sequences = events
            .iter()
            .cloned()
            .map(|event| staged.append(event))
            .collect::<Result<Vec<_>, _>>()?;
        *self = staged;
        Ok(sequences)
    }

    fn events(&self, run_id: RunId) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(self
            .events
            .iter()
            .filter(|stored| stored.event.run_id == run_id)
            .cloned()
            .collect())
    }

    fn events_since(
        &self,
        run_id: RunId,
        sequence: Sequence,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(self
            .events
            .iter()
            .filter(|stored| stored.event.run_id == run_id && stored.sequence > sequence)
            .cloned()
            .collect())
    }

    fn reconstruct(&self, run_id: RunId) -> Result<RuntimeState, StoreError> {
        let events = self.events(run_id)?;
        if events.is_empty() {
            return Err(StoreError::UnknownRun(run_id));
        }

        let mut state = RuntimeState::new(run_id);
        for stored in &events {
            state.apply_event(&stored.event)?;
        }
        state.events_applied = events.len() as u64;
        Ok(state)
    }

    fn snapshot(&self, run_id: RunId) -> Result<RuntimeSnapshot, StoreError> {
        let events = self.events(run_id)?;
        let state = self.reconstruct(run_id)?;
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

impl BranchStore for InMemoryEventStore {
    fn create_branch(
        &mut self,
        parent_run_id: RunId,
        fork_sequence: Sequence,
        replay_mode: ReplayMode,
        created_at_ms: u64,
    ) -> Result<BranchMetadata, StoreError> {
        validate_fork_sequence(&self.events, parent_run_id, fork_sequence)?;
        let metadata = BranchMetadata {
            branch_id: BranchId::new(),
            parent_run_id,
            fork_sequence,
            replay_mode,
            created_at_ms,
        };
        self.branches.insert(metadata.branch_id, metadata.clone());
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

impl SnapshotStore for InMemoryEventStore {
    fn save_snapshot(&mut self, snapshot: &RuntimeSnapshot) -> Result<(), StoreError> {
        validate_snapshot(&self.events, snapshot)?;
        self.snapshots
            .insert((snapshot.run_id, snapshot.at_sequence), snapshot.clone());
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

fn validate_fork_sequence(
    events: &[StoredEvent],
    parent_run_id: RunId,
    fork_sequence: Sequence,
) -> Result<(), StoreError> {
    if !events
        .iter()
        .any(|event| event.event.run_id == parent_run_id)
    {
        return Err(StoreError::UnknownRun(parent_run_id));
    }
    if events
        .iter()
        .any(|event| event.event.run_id == parent_run_id && event.sequence == fork_sequence)
    {
        return Ok(());
    }
    Err(StoreError::InvalidTransition(format!(
        "fork sequence {fork_sequence} is not present in run {parent_run_id}"
    )))
}

fn validate_snapshot(events: &[StoredEvent], snapshot: &RuntimeSnapshot) -> Result<(), StoreError> {
    if snapshot.state.run_id != snapshot.run_id {
        return Err(StoreError::InvalidTransition(
            "snapshot state belongs to another run".to_string(),
        ));
    }
    let prefix: Vec<StoredEvent> = events
        .iter()
        .filter(|event| {
            event.event.run_id == snapshot.run_id && event.sequence <= snapshot.at_sequence
        })
        .cloned()
        .collect();
    let at_sequence_exists = prefix
        .last()
        .is_some_and(|event| event.sequence == snapshot.at_sequence);
    if !at_sequence_exists {
        if !events
            .iter()
            .any(|event| event.event.run_id == snapshot.run_id)
        {
            return Err(StoreError::UnknownRun(snapshot.run_id));
        }
        return Err(StoreError::InvalidTransition(format!(
            "snapshot sequence {} is not present in run {}",
            snapshot.at_sequence, snapshot.run_id
        )));
    }
    let expected_events = prefix.len() as u64;
    if snapshot.state.events_applied != expected_events {
        return Err(StoreError::InvalidTransition(format!(
            "snapshot has {} applied events, expected {expected_events}",
            snapshot.state.events_applied
        )));
    }
    let mut expected_state = RuntimeState::new(snapshot.run_id);
    for event in &prefix {
        expected_state.apply_event(&event.event)?;
    }
    if expected_state != snapshot.state {
        return Err(StoreError::InvalidTransition(format!(
            "snapshot state does not match event prefix at sequence {}",
            snapshot.at_sequence
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedReplay {
    events: Vec<StoredEvent>,
    state: RuntimeState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub run_id: RunId,
    pub at_sequence: Sequence,
    pub state: RuntimeState,
}

impl RecordedReplay {
    pub fn events(&self) -> &[StoredEvent] {
        &self.events
    }

    pub fn state(&self) -> &RuntimeState {
        &self.state
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Active,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskStatus {
    Created,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    Created,
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskState {
    pub id: TaskId,
    pub run_id: RunId,
    pub title: String,
    pub status: TaskStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentState {
    pub identity: AgentIdentity,
    pub status: AgentStatus,
    pub model: ModelRef,
    pub chunks_received: u32,
    pub usage: Usage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeState {
    pub run_id: RunId,
    pub status: RunStatus,
    pub tasks: BTreeMap<TaskId, TaskState>,
    pub agents: BTreeMap<AgentId, AgentState>,
    pub artifacts: BTreeMap<ContentHash, ArtifactRef>,
    pub events_applied: u64,
}

impl RuntimeState {
    fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            status: RunStatus::Active,
            tasks: BTreeMap::new(),
            agents: BTreeMap::new(),
            artifacts: BTreeMap::new(),
            events_applied: 0,
        }
    }

    /// Validate and apply one event to a recovered runtime projection.
    /// Runtime mutation APIs call this before appending because stores persist
    /// opaque domain payloads and cannot validate every cross-domain rule.
    pub fn apply_event(&mut self, event: &Event) -> Result<(), StoreError> {
        if event.run_id != self.run_id {
            return Err(StoreError::InvalidTransition(format!(
                "event {} belongs to run {}, expected {}",
                event.id, event.run_id, self.run_id
            )));
        }

        if self.events_applied == 0 && !matches!(event.kind, EventKind::RunCreated { .. }) {
            return Err(StoreError::InvalidTransition(
                "run.created must be the first event".to_string(),
            ));
        }

        if self.events_applied > 0
            && matches!(
                self.status,
                RunStatus::Completed | RunStatus::Cancelled | RunStatus::Failed
            )
        {
            return Err(StoreError::InvalidTransition(
                "events cannot be appended after a terminal run transition".to_string(),
            ));
        }

        match &event.kind {
            EventKind::RunCreated { run_id } if *run_id == self.run_id => {
                if self.events_applied != 0 {
                    return Err(StoreError::InvalidTransition(
                        "run.created must be the first event".to_string(),
                    ));
                }
            }
            EventKind::RunCreated { run_id } => {
                return Err(StoreError::InvalidTransition(format!(
                    "run.created names {run_id}, expected {}",
                    self.run_id
                )));
            }
            EventKind::TaskCreated {
                task_id,
                run_id,
                title,
            } => {
                if *run_id != self.run_id {
                    return Err(StoreError::InvalidTransition(
                        "task belongs to another run".to_string(),
                    ));
                }
                if self.tasks.contains_key(task_id) {
                    return Err(StoreError::InvalidTransition(format!(
                        "task {task_id} was created twice"
                    )));
                }
                self.tasks.insert(
                    *task_id,
                    TaskState {
                        id: *task_id,
                        run_id: *run_id,
                        title: title.clone(),
                        status: TaskStatus::Created,
                    },
                );
            }
            EventKind::AgentCreated { agent } => {
                if self.agents.contains_key(&agent.id) {
                    return Err(StoreError::InvalidTransition(format!(
                        "agent {} was created twice",
                        agent.id
                    )));
                }
                self.agents.insert(
                    agent.id,
                    AgentState {
                        identity: agent.clone(),
                        status: AgentStatus::Created,
                        model: agent.model.clone(),
                        chunks_received: 0,
                        usage: Usage::default(),
                    },
                );
            }
            EventKind::ModelRequested { agent_id, model } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                if matches!(
                    agent.status,
                    AgentStatus::Completed | AgentStatus::Cancelled | AgentStatus::Failed
                ) {
                    return Err(StoreError::InvalidTransition(format!(
                        "agent {agent_id} cannot receive a model request from {:?}",
                        agent.status
                    )));
                }
                agent.model = model.clone();
                agent.status = AgentStatus::Running;
            }
            EventKind::ModelChunkReceived { agent_id, .. } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                agent.chunks_received = agent.chunks_received.saturating_add(1);
            }
            EventKind::ModelCompleted { agent_id, usage } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                agent.status = AgentStatus::Completed;
                agent.usage = *usage;
            }
            EventKind::ModelCancelled { agent_id } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                agent.status = AgentStatus::Cancelled;
            }
            EventKind::ModelFailed { agent_id, .. } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                agent.status = AgentStatus::Failed;
            }
            EventKind::AgentPaused { agent_id } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                if matches!(
                    agent.status,
                    AgentStatus::Completed
                        | AgentStatus::Cancelled
                        | AgentStatus::Failed
                        | AgentStatus::Paused
                ) {
                    return Err(StoreError::InvalidTransition(format!(
                        "agent {agent_id} cannot be paused from {:?}",
                        agent.status
                    )));
                }
                agent.status = AgentStatus::Paused;
            }
            EventKind::AgentResumed { agent_id } => {
                let agent = self
                    .agents
                    .get_mut(agent_id)
                    .ok_or(StoreError::UnknownAgent(*agent_id))?;
                if agent.status != AgentStatus::Paused {
                    return Err(StoreError::InvalidTransition(format!(
                        "agent {agent_id} is not paused"
                    )));
                }
                agent.status = AgentStatus::Running;
            }
            EventKind::CapabilityTransition { .. } => {
                // Security owns capability payload decoding and replay.
            }
            EventKind::ToolTransition { .. } => {
                // Tool runtime owns transaction decoding and replay.
            }
            EventKind::ContextTransition { .. } => {
                // Context owns the typed payload and replays it separately.
                // The runtime event store preserves its ordering and bytes.
            }
            EventKind::CacheObserved { .. } => {
                // Cache telemetry is a separate projection over durable events.
            }
            EventKind::AgentMessage { .. } => {
                // IPC owns the typed payload and replays it separately.
            }
            EventKind::AssumptionTransition { .. } => {
                // Assumptions own the typed payload and replay it separately.
            }
            EventKind::SchedulerTransition { .. } => {
                // Scheduler owns the typed payload and replays it separately.
            }
            EventKind::FailureMemoryTransition { .. } => {
                // Failure memory owns the typed payload and replays it separately.
            }
            EventKind::SpecialistTransition { .. } => {
                // Specialist profiles own the typed payload and replay it separately.
            }
            EventKind::ArtifactCreated {
                content_hash,
                size_bytes,
                media_type,
                trust,
            } => {
                let content_hash = ContentHash::from_digest(*content_hash);
                if self.artifacts.contains_key(&content_hash) {
                    return Err(StoreError::InvalidTransition(format!(
                        "artifact {content_hash} was created twice"
                    )));
                }
                self.artifacts.insert(
                    content_hash,
                    ArtifactRef {
                        content_hash,
                        size_bytes: *size_bytes,
                        media_type: media_type.clone(),
                        trust: *trust,
                    },
                );
            }
            EventKind::RunCompleted { run_id } if *run_id == self.run_id => {
                self.status = RunStatus::Completed;
            }
            EventKind::RunCancelled { run_id } if *run_id == self.run_id => {
                self.status = RunStatus::Cancelled;
            }
            EventKind::RunFailed { run_id, .. } if *run_id == self.run_id => {
                self.status = RunStatus::Failed;
            }
            EventKind::RunCompleted { .. }
            | EventKind::RunCancelled { .. }
            | EventKind::RunFailed { .. } => {
                return Err(StoreError::InvalidTransition(
                    "terminal event belongs to another run".to_string(),
                ));
            }
        }

        self.events_applied = self.events_applied.saturating_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_agent::AgentSession;
    use orynth_assumptions::{
        ASSUMPTION_SCHEMA_VERSION, Assumption, AssumptionTransition,
        LEGACY_ASSUMPTION_SCHEMA_VERSION, decode_transition, encode_transition,
    };
    use orynth_ipc::{IPC_SCHEMA_VERSION, IpcEnvelope, IpcMessage, LEGACY_IPC_SCHEMA_VERSION};
    use orynth_kernel::{
        AgentIdentity, CancellationToken, ModelClass, ModelRef, Task, ToolTransactionId,
    };
    use orynth_provider::MockProvider;
    use orynth_tool_runtime::{
        TOOL_SCHEMA_VERSION, ToolProposal, ToolProvenance, ToolState, ToolTransition,
        decode_transition as decode_tool_transition, encode_transition as encode_tool_transition,
    };
    const LEGACY_TOOL_SCHEMA_VERSION: u16 = 1;

    fn successful_trace() -> EventTrace {
        let model = ModelRef::new("mock", "runtime-core", ModelClass::Cheap);
        let identity = AgentIdentity::new("runtime-agent", "exercise event storage", model.clone());
        AgentSession::new(
            identity,
            MockProvider::new(model, "stored response").with_chunk_size(3),
        )
        .run("persist this run", CancellationToken::new())
        .expect("mock run should succeed")
        .trace
    }

    #[test]
    fn reconstructs_a_successful_run_from_immutable_events() {
        let trace = successful_trace();
        let run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();

        let sequences = store.append_trace(&trace).expect("trace should append");
        let state = store.reconstruct(run_id).expect("state should reconstruct");

        assert_eq!(sequences.first(), Some(&1));
        assert_eq!(sequences.len(), trace.len());
        assert_eq!(state.status, RunStatus::Completed);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.events_applied as usize, trace.len());

        let agent = state.agents.values().next().expect("agent state");
        assert_eq!(agent.status, AgentStatus::Completed);
        assert_eq!(agent.chunks_received, 5);
        assert_eq!(agent.usage.total_tokens(), 5);
    }

    #[test]
    fn recorded_replay_only_reconstructs_captured_events() {
        let trace = successful_trace();
        let run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");

        let replay = store
            .recorded_replay(run_id)
            .expect("replay should succeed");

        assert_eq!(replay.events().len(), trace.len());
        assert_eq!(replay.state().status, RunStatus::Completed);
    }

    #[test]
    fn duplicate_event_ids_are_rejected() {
        let trace = successful_trace();
        let event = trace.events()[0].clone();
        let mut store = InMemoryEventStore::new();

        store.append(event.clone()).expect("first append");
        assert_eq!(
            store.append(event),
            Err(StoreError::DuplicateEvent(trace.events()[0].id))
        );
    }

    #[test]
    fn terminal_agent_state_rejects_later_model_requests() {
        let run_id = RunId::new();
        let model = ModelRef::new("mock", "cheap", ModelClass::Cheap);
        let agent = AgentIdentity::new("worker", "cancelable", model.clone());
        let mut store = InMemoryEventStore::new();
        store
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run should append");
        store
            .append(Event::new(
                run_id,
                EventKind::AgentCreated {
                    agent: agent.clone(),
                },
            ))
            .expect("agent should append");
        store
            .append(Event::new(
                run_id,
                EventKind::ModelCancelled { agent_id: agent.id },
            ))
            .expect("cancellation should append");
        store
            .append(Event::new(
                run_id,
                EventKind::ModelRequested {
                    agent_id: agent.id,
                    model,
                },
            ))
            .expect("event append remains opaque");
        assert!(matches!(
            store.reconstruct(run_id),
            Err(StoreError::InvalidTransition(_))
        ));
    }

    #[test]
    fn in_memory_batch_append_is_atomic_on_duplicate_ids() {
        let trace = successful_trace();
        let event = trace.events()[0].clone();
        let mut store = InMemoryEventStore::new();

        assert_eq!(
            store.append_batch(&[event.clone(), event.clone()]),
            Err(StoreError::DuplicateEvent(event.id))
        );
        assert!(store.all_events().is_empty());
    }

    #[test]
    fn failed_runs_reconstruct_the_failed_agent_and_run() {
        let model = ModelRef::new("mock", "runtime-core", ModelClass::Cheap);
        let identity = AgentIdentity::new("runtime-agent", "exercise failure", model.clone());
        let error = AgentSession::new(
            identity,
            MockProvider::new(model, "never accepted").with_failure_after_chunks(0),
        )
        .run("fail this run", CancellationToken::new())
        .expect_err("mock provider should fail");
        let run_id = match &error.trace().events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store
            .append_trace(error.trace())
            .expect("failed trace should append");

        let state = store
            .reconstruct(run_id)
            .expect("failed state should reconstruct");

        assert_eq!(state.status, RunStatus::Failed);
        assert_eq!(
            state.agents.values().next().expect("agent").status,
            AgentStatus::Failed
        );
    }

    #[test]
    fn unknown_runs_cannot_be_reconstructed() {
        let store = InMemoryEventStore::new();

        assert_eq!(
            store.reconstruct(RunId::from_u64(999)),
            Err(StoreError::UnknownRun(RunId::from_u64(999)))
        );
    }

    #[test]
    fn reconstruction_rejects_events_before_run_creation() {
        let run_id = RunId::new();
        let mut store = InMemoryEventStore::new();
        let event = Event::new(
            run_id,
            EventKind::TaskCreated {
                task_id: TaskId::new(),
                run_id,
                title: "invalid ordering".to_string(),
            },
        );
        store.append(event).expect("event should be stored");

        assert!(matches!(
            store.reconstruct(run_id),
            Err(StoreError::InvalidTransition(message))
                if message == "run.created must be the first event"
        ));
    }

    #[test]
    fn snapshots_pin_the_last_sequence_and_state() {
        let trace = successful_trace();
        let run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");

        let snapshot = store.snapshot(run_id).expect("snapshot should exist");

        assert_eq!(snapshot.run_id, run_id);
        assert_eq!(snapshot.at_sequence as usize, trace.len());
        assert_eq!(snapshot.state.status, RunStatus::Completed);
        store
            .save_snapshot(&snapshot)
            .expect("snapshot should be stored");
        assert_eq!(
            store
                .load_snapshot(run_id, Some(snapshot.at_sequence))
                .expect("snapshot should load"),
            Some(snapshot.clone())
        );
        assert!(
            store
                .events_since(run_id, snapshot.at_sequence)
                .expect("incremental read should succeed")
                .is_empty()
        );
    }

    #[test]
    fn snapshots_preserve_logical_identity_and_effective_model_after_switches() {
        let run_id = RunId::new();
        let agent_id = AgentId::new();
        let original = ModelRef::new("provider-a", "cheap", ModelClass::Cheap);
        let promoted = ModelRef::new("provider-b", "strong", ModelClass::Strong);
        let demoted = ModelRef::new("provider-c", "local", ModelClass::Local);
        let events = [
            Event::new(run_id, EventKind::RunCreated { run_id }),
            Event::new(
                run_id,
                EventKind::AgentCreated {
                    agent: AgentIdentity {
                        id: agent_id,
                        name: "switchable".to_owned(),
                        mission: "snapshot identity".to_owned(),
                        model: original.clone(),
                    },
                },
            ),
            Event::new(
                run_id,
                EventKind::ModelRequested {
                    agent_id,
                    model: promoted,
                },
            ),
            Event::new(
                run_id,
                EventKind::ModelRequested {
                    agent_id,
                    model: demoted.clone(),
                },
            ),
        ];
        let mut store = InMemoryEventStore::new();
        store
            .append_batch(&events)
            .expect("switch events should append");
        let snapshot = store.snapshot(run_id).expect("snapshot should exist");
        let encoded =
            sqlite::encode_snapshot_state(&snapshot.state).expect("snapshot should encode");
        let decoded = sqlite::decode_snapshot_state(&encoded).expect("snapshot should decode");
        let agent = decoded.agents.get(&agent_id).expect("agent should recover");
        assert_eq!(agent.identity.id, agent_id);
        assert_eq!(agent.identity.model, original);
        assert_eq!(agent.model, demoted);
        store
            .save_snapshot(&snapshot)
            .expect("snapshot should save");
        assert_eq!(
            store.load_snapshot(run_id, None).unwrap().unwrap().state,
            decoded
        );
    }

    #[test]
    fn branches_pin_a_parent_run_sequence_and_mode() {
        let trace = successful_trace();
        let run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");

        let branch = store
            .create_branch(run_id, 2, ReplayMode::ForkLive, 1234)
            .expect("branch should be created");

        assert_eq!(branch.parent_run_id, run_id);
        assert_eq!(branch.fork_sequence, 2);
        assert_eq!(branch.replay_mode, ReplayMode::ForkLive);
        assert_eq!(
            store.branch(branch.branch_id).expect("branch lookup"),
            Some(branch)
        );
        assert_eq!(
            store
                .branches_for_run(run_id)
                .expect("branch listing")
                .len(),
            1
        );
        assert!(matches!(
            store.create_branch(run_id, 0, ReplayMode::Recorded, 1235),
            Err(StoreError::InvalidTransition(_))
        ));
    }

    #[test]
    fn fork_materialization_creates_an_isolated_child_prefix() {
        let trace = successful_trace();
        let parent_run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");
        let branch = store
            .create_branch(parent_run_id, 2, ReplayMode::ForkLive, 2222)
            .expect("branch should be created");
        let child_run_id = RunId::new();

        let fork = store
            .materialize_fork(branch.branch_id, child_run_id)
            .expect("fork should materialize");
        let child = store
            .reconstruct(child_run_id)
            .expect("child state should reconstruct");

        assert_eq!(fork.child_run_id, child_run_id);
        assert_eq!(fork.copied_event_count, 2);
        assert_eq!(fork.child_sequences.len(), 2);
        assert_eq!(child.run_id, child_run_id);
        assert_eq!(child.status, RunStatus::Active);
        assert_eq!(child.tasks.len(), 1);
        assert_eq!(
            store.events(parent_run_id).expect("parent events").len(),
            trace.len()
        );
        assert!(matches!(
            store.materialize_fork(branch.branch_id, child_run_id),
            Err(StoreError::InvalidTransition(_))
        ));
    }

    #[test]
    fn materialized_fork_remaps_typed_ipc_scope() {
        let parent_run_id = RunId::new();
        let sender = orynth_kernel::AgentId::new();
        let recipient = orynth_kernel::AgentId::new();
        let envelope = IpcEnvelope::new(
            parent_run_id,
            None,
            sender,
            recipient,
            IpcMessage::Question {
                subject: "schema.users.id".to_owned(),
                why: "the child needs the same narrow contract question".to_owned(),
            },
        );
        let mut trace = EventTrace::default();
        trace.record(Event::new(
            parent_run_id,
            EventKind::RunCreated {
                run_id: parent_run_id,
            },
        ));
        trace.record(Event::new(
            parent_run_id,
            EventKind::AgentMessage {
                version: orynth_ipc::IPC_SCHEMA_VERSION,
                payload: envelope.encode().expect("message should encode"),
            },
        ));

        let mut store = InMemoryEventStore::new();
        store
            .append_trace(&trace)
            .expect("parent trace should append");
        let branch = store
            .create_branch(parent_run_id, 2, ReplayMode::ForkLive, 4444)
            .expect("branch should be created");
        let child_run_id = RunId::new();
        let fork = store
            .materialize_fork(branch.branch_id, child_run_id)
            .expect("fork should materialize");
        let child_envelope = match &fork.child_prefix.events()[1].kind {
            EventKind::AgentMessage { version, payload } => {
                IpcEnvelope::decode(*version, payload).expect("child message should decode")
            }
            _ => panic!("expected a typed IPC event"),
        };
        assert_eq!(child_envelope.run_id, child_run_id);
        assert_eq!(child_envelope.sender, sender);
        assert_eq!(child_envelope.recipient, recipient);
    }

    #[test]
    fn legacy_fork_payloads_are_reencoded_with_their_current_schema_tags() {
        let parent_run = RunId::new();
        let child_run = RunId::new();
        let agent_id = AgentId::new();
        let model = ModelRef::new("mock", "fork", ModelClass::Cheap);
        let agent = AgentIdentity::new("fork-agent", "legacy remap", model);
        let task = Task::new(parent_run, "legacy fork");

        let envelope = IpcEnvelope::new(
            parent_run,
            Some(task.id),
            agent_id,
            AgentId::new(),
            IpcMessage::Question {
                subject: "legacy".to_owned(),
                why: "remap".to_owned(),
            },
        );
        let mut ipc_payload = envelope.encode().unwrap();
        let input_origin_offset = 8 + 8 + 1 + 8 + 8 + 8 + 2 + 1;
        ipc_payload.drain(input_origin_offset..input_origin_offset + 2);

        let assumption = Assumption::new(
            parent_run,
            agent_id,
            "legacy.subject",
            "value",
            "legacy claim",
        );
        let mut assumption_payload =
            encode_transition(&AssumptionTransition::Created { assumption }).unwrap();
        assumption_payload.truncate(assumption_payload.len() - 3);

        let tool = ToolTransition::Proposed {
            transaction_id: ToolTransactionId::from_u64(77),
            proposal: ToolProposal {
                run_id: parent_run,
                task_id: None,
                agent_id,
                tool_name: "audit.note".to_owned(),
                input: std::collections::BTreeMap::new(),
                provenance: ToolProvenance::Agent,
                input_origins: Vec::new(),
            },
            state: ToolState::Validated,
        };
        let current_tool = encode_tool_transition(&tool).unwrap();
        let mut tool_payload = current_tool[..current_tool.len() - 5].to_vec();
        tool_payload.push(*current_tool.last().unwrap());

        let mut trace = EventTrace::default();
        trace.record(Event::new(
            parent_run,
            EventKind::RunCreated { run_id: parent_run },
        ));
        trace.record(Event::new(
            parent_run,
            EventKind::TaskCreated {
                task_id: task.id,
                run_id: parent_run,
                title: task.title.clone(),
            },
        ));
        trace.record(Event::new(parent_run, EventKind::AgentCreated { agent }));
        trace.record(Event::new(
            parent_run,
            EventKind::AgentMessage {
                version: LEGACY_IPC_SCHEMA_VERSION,
                payload: ipc_payload,
            },
        ));
        trace.record(Event::new(
            parent_run,
            EventKind::AssumptionTransition {
                version: LEGACY_ASSUMPTION_SCHEMA_VERSION,
                payload: assumption_payload,
            },
        ));
        trace.record(Event::new(
            parent_run,
            EventKind::ToolTransition {
                version: LEGACY_TOOL_SCHEMA_VERSION,
                payload: tool_payload,
            },
        ));

        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).unwrap();
        let branch = store
            .create_branch(parent_run, trace.len() as u64, ReplayMode::Recorded, 4)
            .unwrap();
        let fork = store.materialize_fork(branch.branch_id, child_run).unwrap();

        let child_events = fork.child_prefix.events();
        let EventKind::AgentMessage { version, payload } = &child_events[3].kind else {
            panic!("expected remapped IPC event");
        };
        assert_eq!(*version, IPC_SCHEMA_VERSION);
        assert_eq!(
            IpcEnvelope::decode(*version, payload).unwrap().run_id,
            child_run
        );
        let EventKind::AssumptionTransition { version, payload } = &child_events[4].kind else {
            panic!("expected remapped assumption event");
        };
        assert_eq!(*version, ASSUMPTION_SCHEMA_VERSION);
        let AssumptionTransition::Created { assumption } =
            decode_transition(*version, payload).unwrap()
        else {
            panic!("expected created assumption");
        };
        assert_eq!(assumption.run_id, child_run);
        let EventKind::ToolTransition { version, payload } = &child_events[5].kind else {
            panic!("expected remapped tool event");
        };
        assert_eq!(*version, TOOL_SCHEMA_VERSION);
        let ToolTransition::Proposed { proposal, .. } =
            decode_tool_transition(*version, payload).unwrap()
        else {
            panic!("expected proposed tool event");
        };
        assert_eq!(proposal.run_id, child_run);

        assert!(matches!(
            &store.events(parent_run).unwrap()[3].event.kind,
            EventKind::AgentMessage { version, .. } if *version == LEGACY_IPC_SCHEMA_VERSION
        ));
    }

    #[test]
    fn failed_fork_remaps_preserve_opaque_payload_schema_tags() {
        let parent_run = RunId::new();
        let child_run = RunId::new();
        let opaque = vec![0xff, 0x00, 0x7f];

        let remapped_ipc = remap_event_for_run(
            &Event::new(
                parent_run,
                EventKind::AgentMessage {
                    version: LEGACY_IPC_SCHEMA_VERSION,
                    payload: opaque.clone(),
                },
            ),
            child_run,
        );
        assert!(matches!(
            remapped_ipc.kind,
            EventKind::AgentMessage { version, payload }
                if version == LEGACY_IPC_SCHEMA_VERSION && payload == opaque
        ));

        let remapped_assumption = remap_event_for_run(
            &Event::new(
                parent_run,
                EventKind::AssumptionTransition {
                    version: LEGACY_ASSUMPTION_SCHEMA_VERSION,
                    payload: opaque.clone(),
                },
            ),
            child_run,
        );
        assert!(matches!(
            remapped_assumption.kind,
            EventKind::AssumptionTransition { version, payload }
                if version == LEGACY_ASSUMPTION_SCHEMA_VERSION && payload == opaque
        ));

        let remapped_tool = remap_event_for_run(
            &Event::new(
                parent_run,
                EventKind::ToolTransition {
                    version: LEGACY_TOOL_SCHEMA_VERSION,
                    payload: opaque.clone(),
                },
            ),
            child_run,
        );
        assert!(matches!(
            remapped_tool.kind,
            EventKind::ToolTransition { version, payload }
                if version == LEGACY_TOOL_SCHEMA_VERSION && payload == opaque
        ));
    }

    #[test]
    fn materialized_fork_remaps_assumption_run_scope() {
        let parent_run_id = RunId::new();
        let owner = orynth_kernel::AgentId::new();
        let assumption = Assumption::new(
            parent_run_id,
            owner,
            "schema.users.id",
            "UUID",
            "users.id is UUID",
        );
        let mut trace = EventTrace::default();
        trace.record(Event::new(
            parent_run_id,
            EventKind::RunCreated {
                run_id: parent_run_id,
            },
        ));
        trace.record(Event::new(
            parent_run_id,
            EventKind::AssumptionTransition {
                version: ASSUMPTION_SCHEMA_VERSION,
                payload: encode_transition(&AssumptionTransition::Created {
                    assumption: assumption.clone(),
                })
                .expect("assumption should encode"),
            },
        ));

        let mut store = InMemoryEventStore::new();
        store
            .append_trace(&trace)
            .expect("parent trace should append");
        let branch = store
            .create_branch(parent_run_id, 2, ReplayMode::ForkLive, 5555)
            .expect("branch should be created");
        let child_run_id = RunId::new();
        let fork = store
            .materialize_fork(branch.branch_id, child_run_id)
            .expect("fork should materialize");
        let EventKind::AssumptionTransition { version, payload } =
            &fork.child_prefix.events()[1].kind
        else {
            panic!("expected an assumption transition event");
        };
        let AssumptionTransition::Created { assumption: child } =
            decode_transition(*version, payload).expect("child assumption should decode")
        else {
            panic!("expected an assumption creation transition");
        };
        assert_eq!(child.run_id, child_run_id);
        assert_eq!(child.owner, owner);
    }

    #[test]
    fn materialized_fork_remaps_tool_proposal_run_scope() {
        let parent_run_id = RunId::new();
        let agent_id = orynth_kernel::AgentId::new();
        let transaction_id = orynth_kernel::ToolTransactionId::new();
        let transition = ToolTransition::Proposed {
            transaction_id,
            proposal: ToolProposal {
                run_id: parent_run_id,
                task_id: None,
                agent_id,
                tool_name: "audit.note".to_owned(),
                input: [("message".to_owned(), "fork me".to_owned())]
                    .into_iter()
                    .collect(),
                provenance: ToolProvenance::Manager,
                input_origins: Vec::new(),
            },
            state: ToolState::Validated,
        };
        let mut trace = EventTrace::default();
        trace.record(Event::new(
            parent_run_id,
            EventKind::RunCreated {
                run_id: parent_run_id,
            },
        ));
        trace.record(Event::new(
            parent_run_id,
            EventKind::ToolTransition {
                version: TOOL_SCHEMA_VERSION,
                payload: encode_tool_transition(&transition)
                    .expect("tool transition should encode"),
            },
        ));

        let mut store = InMemoryEventStore::new();
        store
            .append_trace(&trace)
            .expect("parent trace should append");
        let branch = store
            .create_branch(parent_run_id, 2, ReplayMode::ForkLive, 6666)
            .expect("branch should be created");
        let child_run_id = RunId::new();
        let fork = store
            .materialize_fork(branch.branch_id, child_run_id)
            .expect("fork should materialize");
        let EventKind::ToolTransition { version, payload } = &fork.child_prefix.events()[1].kind
        else {
            panic!("expected a tool transition event");
        };
        let ToolTransition::Proposed { proposal, .. } =
            decode_tool_transition(*version, payload).expect("child tool transition should decode")
        else {
            panic!("expected a tool proposal transition");
        };
        assert_eq!(proposal.run_id, child_run_id);
    }

    #[test]
    fn materialized_fork_can_continue_with_a_provider_and_persist_only_new_events() {
        let trace = successful_trace();
        let parent_run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");
        let branch = store
            .create_branch(parent_run_id, 3, ReplayMode::ForkLive, 3333)
            .expect("branch should be created");
        let child_run_id = RunId::new();
        let fork = store
            .materialize_fork(branch.branch_id, child_run_id)
            .expect("fork should materialize");
        assert_eq!(fork.copied_event_count, 3);
        let identity = match &fork.child_prefix.events()[2].kind {
            EventKind::AgentCreated { agent } => agent.clone(),
            _ => panic!("fork prefix should contain agent.created"),
        };
        let task = match &fork.child_prefix.events()[1].kind {
            EventKind::TaskCreated {
                task_id,
                run_id,
                title,
            } => Task {
                id: *task_id,
                run_id: *run_id,
                title: title.clone(),
            },
            _ => panic!("fork prefix should contain task.created"),
        };
        let session = AgentSession::new(
            identity.clone(),
            MockProvider::new(identity.model.clone(), "continued response"),
        );
        let execution = session
            .run_fork(
                child_run_id,
                task,
                "continue this child run",
                fork.child_prefix.clone(),
                CancellationToken::new(),
            )
            .expect("fork should continue through provider");
        let continuation_sequences = persist_fork_continuation(&mut store, &fork, &execution.trace)
            .expect("new fork events should append atomically");
        assert_eq!(
            continuation_sequences.len(),
            execution.trace.len() - fork.child_prefix.len()
        );

        let child = store
            .reconstruct(child_run_id)
            .expect("continued child should reconstruct");
        assert_eq!(child.status, RunStatus::Completed);
        assert_eq!(
            child.agents.get(&identity.id).expect("child agent").status,
            AgentStatus::Completed
        );
        assert_eq!(
            store.events(parent_run_id).expect("parent events").len(),
            trace.len()
        );
    }

    #[test]
    fn incremental_reads_return_only_later_events() {
        let trace = successful_trace();
        let run_id = match &trace.events()[0].kind {
            EventKind::RunCreated { run_id } => *run_id,
            _ => panic!("trace must begin with run.created"),
        };
        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");

        let later = store
            .events_since(run_id, 2)
            .expect("incremental read should succeed");

        assert_eq!(later.len(), trace.len() - 2);
        assert!(later.iter().all(|event| event.sequence > 2));
    }

    #[test]
    fn artifact_store_deduplicates_immutable_content() {
        let mut artifacts = InMemoryArtifactStore::new();
        let first = artifacts
            .put("text/plain", b"same bytes".to_vec())
            .expect("artifact should be stored");
        let second = artifacts
            .put("text/plain", b"same bytes".to_vec())
            .expect("same artifact should deduplicate");

        assert_eq!(first, second);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(
            artifacts
                .get(first.content_hash)
                .expect("artifact lookup should succeed")
                .expect("artifact should be readable")
                .bytes(),
            b"same bytes"
        );
    }

    #[test]
    fn artifact_provenance_combines_without_upgrading_trust() {
        let mut artifacts = InMemoryArtifactStore::new();
        let trusted = artifacts
            .put_with_trust(
                "text/plain",
                b"same bytes".to_vec(),
                TrustOrigin::TrustedProject,
            )
            .expect("artifact should be stored");
        assert_eq!(trusted.trust, TrustOrigin::TrustedProject);
        let untrusted = artifacts
            .put_with_trust(
                "text/plain",
                b"same bytes".to_vec(),
                TrustOrigin::WebUntrusted,
            )
            .expect("same artifact should deduplicate");
        assert_eq!(untrusted.trust, TrustOrigin::WebUntrusted);
        assert_eq!(
            artifacts
                .get(trusted.content_hash)
                .expect("artifact lookup should succeed")
                .expect("artifact should be readable")
                .reference
                .trust,
            TrustOrigin::WebUntrusted
        );
    }

    #[test]
    fn artifact_payloads_have_an_explicit_inline_bound() {
        let mut artifacts = InMemoryArtifactStore::new();
        let oversized = vec![0_u8; MAX_ARTIFACT_BYTES + 1];
        assert!(matches!(
            artifacts.put("application/octet-stream", oversized),
            Err(ArtifactError::TooLarge { actual, maximum })
                if actual == MAX_ARTIFACT_BYTES + 1 && maximum == MAX_ARTIFACT_BYTES
        ));
        assert!(artifacts.is_empty());
    }

    #[test]
    fn artifact_creation_is_reconstructable_without_payload_duplication() {
        let run_id = RunId::new();
        let mut artifacts = InMemoryArtifactStore::new();
        let reference = artifacts
            .put("application/octet-stream", vec![1, 2, 3, 4])
            .expect("artifact should be stored");
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(reference.created_event(run_id));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        let mut store = InMemoryEventStore::new();
        store.append_trace(&trace).expect("trace should append");
        let state = store.reconstruct(run_id).expect("state should reconstruct");

        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(
            state
                .artifacts
                .get(&reference.content_hash)
                .expect("artifact reference")
                .size_bytes,
            4
        );
        assert_eq!(
            artifacts
                .get(reference.content_hash)
                .expect("artifact lookup should succeed")
                .expect("payload remains in blob store")
                .bytes(),
            &[1, 2, 3, 4]
        );
    }
}
