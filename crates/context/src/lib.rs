//! Typed, versioned context state for Orynth.
//!
//! The graph owns metadata and immutable content references. Prompt rendering
//! is a projection of that graph; it is not the graph itself.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use orynth_kernel::{AgentId, Event, EventId, EventKind, RunId};

pub use orynth_kernel::TrustOrigin as TrustLevel;

pub const CONTEXT_TRANSITION_VERSION: u16 = 1;
const MAX_CONTEXT_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONTEXT_ITEMS: usize = 1_000_000;

static NEXT_CONTEXT_BLOCK_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContextBlockId(u64);

impl ContextBlockId {
    pub fn new() -> Self {
        Self(NEXT_CONTEXT_BLOCK_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl Default for ContextBlockId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ContextBlockId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "context-{:016x}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    fn new() -> Self {
        Self(NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed))
    }
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

/// Deterministic identity hashing for local context addressing.
///
/// This is intentionally not cryptographic and must not be used as a trust or
/// security boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeterministicContentHasher;

impl DeterministicContentHasher {
    pub fn hash(self, bytes: &[u8]) -> ContentHash {
        let mut lanes = [
            0xcbf29ce484222325_u64,
            0x84222325cb29ce4_u64,
            0x9e3779b185ebca87_u64,
            0xd6e8feb86659fd93_u64,
        ];
        let multipliers = [
            0x100000001b3_u64,
            0x100000001b3_u64 ^ 0x9e3779b97f4a7c15,
            0x100000001b3_u64 ^ 0xd6e8feb86659fd93,
            0x100000001b3_u64 ^ 0xa0761d6478bd642f,
        ];
        for (index, byte) in bytes.iter().copied().enumerate() {
            let lane = index & 3;
            lanes[lane] ^= u64::from(byte);
            lanes[lane] = lanes[lane].wrapping_mul(multipliers[lane]);
            lanes[lane] ^= lanes[(lane + 1) & 3].rotate_left(13);
        }

        let mut digest = [0_u8; 32];
        for (index, lane) in lanes.iter().enumerate() {
            digest[index * 8..(index + 1) * 8].copy_from_slice(&lane.to_le_bytes());
        }
        ContentHash(digest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextKind {
    Note,
    Project,
    Contract,
    Task,
    Result,
    ArtifactReference,
    Custom(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextScope {
    Global,
    Team,
    Private(AgentId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextOwner {
    Runtime,
    Agent(AgentId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextTrustPolicy {
    AllowAll,
    ExcludeExternal,
    TrustedOnly,
}

impl ContextTrustPolicy {
    fn allows(self, trust: TrustLevel) -> bool {
        match self {
            Self::AllowAll => true,
            Self::ExcludeExternal => matches!(
                trust,
                TrustLevel::TrustedProject | TrustLevel::UserProvided | TrustLevel::Generated
            ),
            Self::TrustedOnly => {
                matches!(trust, TrustLevel::TrustedProject | TrustLevel::UserProvided)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextLifecycle {
    Active,
    Stale,
    Superseded,
    Archived,
    Invalidated,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContextRef {
    pub block_id: ContextBlockId,
    pub revision: u64,
}

impl fmt::Display for ContextRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.block_id, self.revision)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextBlock {
    pub id: ContextBlockId,
    pub revision: u64,
    pub namespace: String,
    pub kind: ContextKind,
    pub owner: ContextOwner,
    pub scope: ContextScope,
    pub content_hash: ContentHash,
    pub token_estimate: u64,
    pub trust: TrustLevel,
    pub importance: u8,
    pub lifecycle: ContextLifecycle,
    pub pinned: bool,
    pub dependencies: Vec<ContextRef>,
    pub sources: Vec<ContextRef>,
    pub created_event: Option<EventId>,
    pub last_access_ms: Option<u64>,
}

impl ContextBlock {
    pub fn reference(&self) -> ContextRef {
        ContextRef {
            block_id: self.id,
            revision: self.revision,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextDraft {
    pub namespace: String,
    pub kind: ContextKind,
    pub owner: ContextOwner,
    pub scope: ContextScope,
    pub content: Vec<u8>,
    pub token_estimate: u64,
    pub trust: TrustLevel,
    pub importance: u8,
    pub dependencies: Vec<ContextRef>,
    pub sources: Vec<ContextRef>,
    pub created_event: Option<EventId>,
}

impl ContextDraft {
    pub fn new(
        namespace: impl Into<String>,
        kind: ContextKind,
        owner: ContextOwner,
        scope: ContextScope,
        content: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            kind,
            owner,
            scope,
            content: content.into(),
            token_estimate: 0,
            trust: TrustLevel::Generated,
            importance: 50,
            dependencies: Vec::new(),
            sources: Vec::new(),
            created_event: None,
        }
    }

    pub fn with_dependency(mut self, dependency: ContextRef) -> Self {
        self.dependencies.push(dependency);
        self
    }

    pub fn with_token_estimate(mut self, token_estimate: u64) -> Self {
        self.token_estimate = token_estimate;
        self
    }

    pub fn with_trust(mut self, trust: TrustLevel) -> Self {
        self.trust = trust;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invalidation {
    pub block: ContextRef,
    pub dependency: Option<ContextRef>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextTransition {
    Created {
        block: ContextBlock,
        content: Vec<u8>,
    },
    CreatedFromArtifact {
        block: ContextBlock,
        artifact_hash: [u8; 32],
    },
    Superseded(ContextRef),
    Archived(ContextRef),
    Restored(ContextRef),
    Pinned(ContextRef),
    Unpinned(ContextRef),
    Invalidated {
        block: ContextRef,
        dependency: Option<ContextRef>,
        reason: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextEventLog {
    events: Vec<ContextTransition>,
}

impl ContextEventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, publication: &ContextPublication) {
        self.events.extend(publication.transitions.iter().cloned());
    }

    pub fn events(&self) -> &[ContextTransition] {
        &self.events
    }

    pub fn replay(&self) -> Result<ContextGraph, ContextError> {
        ContextGraph::replay(self.events.iter().cloned())
    }

    pub fn to_events(&self, run_id: RunId) -> Result<Vec<Event>, ContextError> {
        self.events
            .iter()
            .map(|transition| {
                Ok(Event::new(
                    run_id,
                    EventKind::ContextTransition {
                        version: CONTEXT_TRANSITION_VERSION,
                        payload: encode_transition(transition)?,
                    },
                ))
            })
            .collect()
    }

    pub fn from_events(events: &[Event]) -> Result<Self, ContextError> {
        let mut log = Self::new();
        for event in events {
            let EventKind::ContextTransition { version, payload } = &event.kind else {
                continue;
            };
            log.events
                .push(decode_transition(*version, payload.as_slice())?);
        }
        Ok(log)
    }

    pub fn from_transitions(transitions: impl IntoIterator<Item = ContextTransition>) -> Self {
        Self {
            events: transitions.into_iter().collect(),
        }
    }

    pub fn replay_with_content<F>(&self, resolver: F) -> Result<ContextGraph, ContextError>
    where
        F: FnMut([u8; 32]) -> Result<Vec<u8>, ContextError>,
    {
        ContextGraph::replay_with_content(self.events.iter().cloned(), resolver)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextPublication {
    pub block: ContextBlock,
    pub transitions: Vec<ContextTransition>,
    pub invalidations: Vec<Invalidation>,
    pub awakened_agents: BTreeSet<AgentId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSubscription {
    pub id: SubscriptionId,
    pub agent_id: AgentId,
    pub namespace_pattern: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionRequest {
    pub namespace_patterns: Vec<String>,
    pub include_stale: bool,
    pub max_blocks: Option<usize>,
    pub max_tokens: Option<u64>,
    pub trust_policy: ContextTrustPolicy,
}

/// Bounded policy inputs for context proprioception.
///
/// The policy only evaluates freshness pressure. It never archives, restores,
/// or invalidates a block by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextFreshnessPolicy {
    pub max_active_tokens: Option<u64>,
    pub max_stale_blocks: Option<usize>,
    pub largest_block_limit: usize,
    pub recent_invalidation_limit: usize,
}

impl Default for ContextFreshnessPolicy {
    fn default() -> Self {
        Self {
            max_active_tokens: None,
            max_stale_blocks: None,
            largest_block_limit: 8,
            recent_invalidation_limit: 8,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextBlockSummary {
    pub reference: ContextRef,
    pub namespace: String,
    pub lifecycle: ContextLifecycle,
    pub pinned: bool,
    pub token_estimate: u64,
    pub importance: u8,
    pub last_access_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSearchRequest {
    pub query: String,
    pub namespace_patterns: Vec<String>,
    pub include_stale: bool,
    pub include_archived: bool,
    pub max_results: usize,
    pub trust_policy: ContextTrustPolicy,
}

impl ContextSearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            namespace_patterns: vec!["*".to_string()],
            include_stale: false,
            include_archived: false,
            max_results: 32,
            trust_policy: ContextTrustPolicy::AllowAll,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextSearchHit {
    pub summary: ContextBlockSummary,
    pub matched_namespace: bool,
    pub matched_content: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextDependencyReport {
    pub reference: ContextRef,
    pub dependencies: Vec<ContextRef>,
    pub sources: Vec<ContextRef>,
    pub dependents: Vec<ContextRef>,
    pub dependents_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextProprioception {
    pub active_blocks: usize,
    pub active_tokens: u64,
    pub stale_blocks: usize,
    pub archived_blocks: usize,
    pub invalidated_blocks: usize,
    pub pinned_blocks: usize,
    pub largest_blocks: Vec<ContextBlockSummary>,
    pub recent_invalidations: Vec<Invalidation>,
    pub active_tokens_over_budget: bool,
    pub stale_blocks_over_budget: bool,
}

impl ProjectionRequest {
    pub fn all() -> Self {
        Self {
            namespace_patterns: vec!["*".to_string()],
            include_stale: false,
            max_blocks: None,
            max_tokens: None,
            trust_policy: ContextTrustPolicy::AllowAll,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPrincipal {
    Runtime,
    Agent(AgentId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedContextBlock {
    pub block: ContextBlock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextProjection {
    pub blocks: Vec<ProjectedContextBlock>,
    pub estimated_tokens: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptLayer {
    pub name: String,
    pub references: Vec<ContextRef>,
    pub stable: bool,
}

impl PromptLayer {
    pub fn stable(name: impl Into<String>, references: Vec<ContextRef>) -> Self {
        Self {
            name: name.into(),
            references,
            stable: true,
        }
    }

    pub fn volatile(name: impl Into<String>, references: Vec<ContextRef>) -> Self {
        Self {
            name: name.into(),
            references,
            stable: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPrompt {
    pub text: String,
    pub stable_prefix_hash: ContentHash,
    pub stable_prefix_bytes: usize,
    pub estimated_tokens: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextError {
    EmptyNamespace,
    InvalidPrivateScope,
    UnknownBlock(ContextRef),
    UnknownContent(ContentHash),
    HashCollision(ContentHash),
    MissingArtifact([u8; 32]),
    ArtifactResolution(String),
    NotVisible(ContextRef),
    NotTrusted(ContextRef),
    TrustUpgrade {
        block: ContextRef,
        dependency: ContextRef,
    },
    InvalidLifecycle {
        block: ContextRef,
        operation: &'static str,
    },
    InvalidSubscriptionPattern,
    InvalidSearchQuery(String),
    InvalidEncoding(String),
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNamespace => formatter.write_str("context namespace must not be empty"),
            Self::InvalidPrivateScope => {
                formatter.write_str("private context scope must match its agent owner")
            }
            Self::UnknownBlock(reference) => write!(formatter, "unknown context block {reference}"),
            Self::UnknownContent(hash) => write!(formatter, "unknown context content {hash}"),
            Self::HashCollision(hash) => write!(formatter, "context hash collision for {hash}"),
            Self::MissingArtifact(hash) => {
                write!(formatter, "context artifact {hash:?} is unavailable")
            }
            Self::ArtifactResolution(message) => {
                write!(formatter, "context artifact resolution failed: {message}")
            }
            Self::NotVisible(reference) => {
                write!(formatter, "context block {reference} is not visible")
            }
            Self::NotTrusted(reference) => {
                write!(
                    formatter,
                    "context block {reference} is not trusted for this projection"
                )
            }
            Self::TrustUpgrade { block, dependency } => write!(
                formatter,
                "context block {block} upgrades trust above dependency {dependency}"
            ),
            Self::InvalidLifecycle { block, operation } => {
                write!(
                    formatter,
                    "cannot {operation} context block {block} in its current lifecycle"
                )
            }
            Self::InvalidSubscriptionPattern => {
                formatter.write_str("context subscription pattern must not be empty")
            }
            Self::InvalidSearchQuery(message) => {
                write!(formatter, "invalid context search: {message}")
            }
            Self::InvalidEncoding(message) => {
                write!(formatter, "invalid context encoding: {message}")
            }
        }
    }
}

impl std::error::Error for ContextError {}

#[derive(Clone, Debug, Default)]
pub struct ContextGraph {
    blocks: BTreeMap<ContextBlockId, ContextBlock>,
    content: BTreeMap<ContentHash, Vec<u8>>,
    namespaces: BTreeMap<String, Vec<ContextRef>>,
    subscriptions: BTreeMap<SubscriptionId, ContextSubscription>,
    invalidations: Vec<Invalidation>,
}

impl ContextGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn namespace_count(&self) -> usize {
        self.namespaces.len()
    }

    pub fn subscription_count(&self) -> usize {
        self.subscriptions.len()
    }

    pub fn invalidation_count(&self) -> usize {
        self.invalidations.len()
    }

    /// Return a compact freshness dashboard for model/operator inspection.
    ///
    /// Blocks and invalidations are bounded by the supplied policy. The
    /// returned pressure flags are advisory; mutation remains an explicit
    /// runtime operation.
    pub fn proprioception(&self, policy: ContextFreshnessPolicy) -> ContextProprioception {
        let mut active_blocks = 0;
        let mut active_tokens = 0_u64;
        let mut stale_blocks = 0;
        let mut archived_blocks = 0;
        let mut invalidated_blocks = 0;
        let mut pinned_blocks = 0;
        let mut summaries = Vec::with_capacity(self.blocks.len());

        for block in self.blocks.values() {
            match block.lifecycle {
                ContextLifecycle::Active => {
                    active_blocks += 1;
                    active_tokens = active_tokens.saturating_add(block.token_estimate);
                }
                ContextLifecycle::Stale => stale_blocks += 1,
                ContextLifecycle::Archived => archived_blocks += 1,
                ContextLifecycle::Invalidated => invalidated_blocks += 1,
                ContextLifecycle::Superseded => {}
            }
            if block.pinned {
                pinned_blocks += 1;
            }
            summaries.push(ContextBlockSummary {
                reference: block.reference(),
                namespace: block.namespace.clone(),
                lifecycle: block.lifecycle,
                pinned: block.pinned,
                token_estimate: block.token_estimate,
                importance: block.importance,
                last_access_ms: block.last_access_ms,
            });
        }

        summaries.sort_by(|left, right| {
            right
                .token_estimate
                .cmp(&left.token_estimate)
                .then_with(|| right.importance.cmp(&left.importance))
                .then_with(|| left.reference.cmp(&right.reference))
        });
        summaries.truncate(policy.largest_block_limit);

        let mut recent_invalidations = self
            .invalidations
            .iter()
            .rev()
            .take(policy.recent_invalidation_limit)
            .cloned()
            .collect::<Vec<_>>();
        recent_invalidations.reverse();

        ContextProprioception {
            active_blocks,
            active_tokens,
            stale_blocks,
            archived_blocks,
            invalidated_blocks,
            pinned_blocks,
            largest_blocks: summaries,
            recent_invalidations,
            active_tokens_over_budget: policy
                .max_active_tokens
                .is_some_and(|limit| active_tokens > limit),
            stale_blocks_over_budget: policy
                .max_stale_blocks
                .is_some_and(|limit| stale_blocks > limit),
        }
    }

    /// Search visible context metadata and bounded text content without
    /// mutating access timestamps or lifecycle state.
    pub fn search(
        &self,
        principal: ContextPrincipal,
        request: &ContextSearchRequest,
    ) -> Result<Vec<ContextSearchHit>, ContextError> {
        const MAX_QUERY_BYTES: usize = 512;
        const MAX_RESULTS: usize = 256;
        if request.query.trim().is_empty() {
            return Err(ContextError::InvalidSearchQuery(
                "query must not be empty".to_string(),
            ));
        }
        if request.query.len() > MAX_QUERY_BYTES {
            return Err(ContextError::InvalidSearchQuery(
                "query exceeds the bounded search length".to_string(),
            ));
        }
        let query = request.query.to_lowercase();
        let limit = request.max_results.min(MAX_RESULTS);
        let mut hits = Vec::new();
        for block in self.blocks.values() {
            if !request
                .namespace_patterns
                .iter()
                .any(|pattern| matches_namespace(pattern, &block.namespace))
                || !can_view(principal, block)
                || !request.trust_policy.allows(block.trust)
            {
                continue;
            }
            let lifecycle_visible = match block.lifecycle {
                ContextLifecycle::Active => true,
                ContextLifecycle::Stale => request.include_stale,
                ContextLifecycle::Archived => request.include_archived,
                ContextLifecycle::Superseded | ContextLifecycle::Invalidated => false,
            };
            if !lifecycle_visible {
                continue;
            }
            let matched_namespace = block.namespace.to_lowercase().contains(&query);
            let matched_content = self
                .content(block.content_hash)
                .map(|content| {
                    String::from_utf8_lossy(content)
                        .to_lowercase()
                        .contains(&query)
                })
                .unwrap_or(false);
            if matched_namespace || matched_content {
                hits.push(ContextSearchHit {
                    summary: ContextBlockSummary {
                        reference: block.reference(),
                        namespace: block.namespace.clone(),
                        lifecycle: block.lifecycle,
                        pinned: block.pinned,
                        token_estimate: block.token_estimate,
                        importance: block.importance,
                        last_access_ms: block.last_access_ms,
                    },
                    matched_namespace,
                    matched_content,
                });
                if hits.len() > limit {
                    hits.sort_by(|left, right| {
                        right
                            .summary
                            .importance
                            .cmp(&left.summary.importance)
                            .then_with(|| left.summary.reference.cmp(&right.summary.reference))
                    });
                    hits.truncate(limit);
                }
            }
        }
        hits.sort_by(|left, right| {
            right
                .summary
                .importance
                .cmp(&left.summary.importance)
                .then_with(|| left.summary.reference.cmp(&right.summary.reference))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Return bounded direct dependency/source and dependent references.
    pub fn dependency_report(
        &self,
        reference: ContextRef,
        max_dependents: usize,
    ) -> Result<ContextDependencyReport, ContextError> {
        let block = self.block(reference)?;
        let mut dependents = self
            .blocks
            .values()
            .filter(|candidate| {
                candidate.dependencies.contains(&reference)
                    || candidate.sources.contains(&reference)
            })
            .map(ContextBlock::reference)
            .collect::<Vec<_>>();
        dependents.sort();
        let dependents_truncated = dependents.len() > max_dependents;
        dependents.truncate(max_dependents);
        Ok(ContextDependencyReport {
            reference,
            dependencies: block.dependencies.clone(),
            sources: block.sources.clone(),
            dependents,
            dependents_truncated,
        })
    }

    pub fn publish(&mut self, draft: ContextDraft) -> Result<ContextPublication, ContextError> {
        if draft.namespace.trim().is_empty() {
            return Err(ContextError::EmptyNamespace);
        }
        if let ContextScope::Private(agent_id) = draft.scope
            && draft.owner != ContextOwner::Agent(agent_id)
        {
            return Err(ContextError::InvalidPrivateScope);
        }
        for dependency in draft.dependencies.iter().chain(draft.sources.iter()) {
            if !self.blocks.contains_key(&dependency.block_id) {
                return Err(ContextError::UnknownBlock(*dependency));
            }
        }
        let trust = draft
            .dependencies
            .iter()
            .chain(draft.sources.iter())
            .try_fold(draft.trust, |trust, dependency| {
                Ok::<_, ContextError>(trust.combine(self.block(*dependency)?.trust))
            })?;

        let previous = self
            .namespaces
            .get(&draft.namespace)
            .and_then(|versions| versions.last().copied());
        let revision = previous.map_or(1, |reference| reference.revision + 1);
        let content_hash = DeterministicContentHasher.hash(&draft.content);
        if let Some(existing) = self.content.get(&content_hash) {
            if existing != &draft.content {
                return Err(ContextError::HashCollision(content_hash));
            }
        } else {
            self.content.insert(content_hash, draft.content);
        }

        if let Some(previous) = previous
            && let Some(block) = self.blocks.get_mut(&previous.block_id)
        {
            block.lifecycle = ContextLifecycle::Superseded;
        }

        let content = self
            .content
            .get(&content_hash)
            .expect("content was inserted or already present")
            .clone();
        let block = ContextBlock {
            id: ContextBlockId::new(),
            revision,
            namespace: draft.namespace.clone(),
            kind: draft.kind,
            owner: draft.owner,
            scope: draft.scope,
            content_hash,
            token_estimate: if draft.token_estimate == 0 {
                estimate_tokens(&content)
            } else {
                draft.token_estimate
            },
            trust,
            importance: draft.importance,
            lifecycle: ContextLifecycle::Active,
            pinned: false,
            dependencies: draft.dependencies,
            sources: draft.sources,
            created_event: draft.created_event,
            last_access_ms: None,
        };
        let reference = block.reference();
        self.blocks.insert(block.id, block.clone());
        self.namespaces
            .entry(draft.namespace)
            .or_default()
            .push(reference);

        let mut invalidations = Vec::new();
        if let Some(previous) = previous {
            invalidations.extend(
                self.mark_dependents_stale(
                    previous,
                    format!("dependency {previous} was superseded"),
                ),
            );
        }
        self.invalidations.extend(invalidations.iter().cloned());

        let mut transitions = vec![ContextTransition::Created {
            block: block.clone(),
            content: content.clone(),
        }];
        if let Some(previous) = previous {
            transitions.push(ContextTransition::Superseded(previous));
        }
        transitions.extend(invalidations.iter().cloned().map(|invalidation| {
            ContextTransition::Invalidated {
                block: invalidation.block,
                dependency: invalidation.dependency,
                reason: invalidation.reason,
            }
        }));

        let awakened_agents = self
            .subscriptions
            .values()
            .filter(|subscription| {
                matches_namespace(&subscription.namespace_pattern, &block.namespace)
                    && can_view(ContextPrincipal::Agent(subscription.agent_id), &block)
            })
            .map(|subscription| subscription.agent_id)
            .collect();

        Ok(ContextPublication {
            block: self.blocks[&reference.block_id].clone(),
            transitions,
            invalidations,
            awakened_agents,
        })
    }

    pub fn replay(
        transitions: impl IntoIterator<Item = ContextTransition>,
    ) -> Result<Self, ContextError> {
        Self::replay_with_content(transitions, |artifact_hash| {
            Err(ContextError::MissingArtifact(artifact_hash))
        })
    }

    pub fn replay_with_content<F>(
        transitions: impl IntoIterator<Item = ContextTransition>,
        mut resolver: F,
    ) -> Result<Self, ContextError>
    where
        F: FnMut([u8; 32]) -> Result<Vec<u8>, ContextError>,
    {
        let mut graph = Self::new();
        for transition in transitions {
            graph.apply_transition_with_content(transition, &mut resolver)?;
        }
        Ok(graph)
    }

    pub fn block(&self, reference: ContextRef) -> Result<&ContextBlock, ContextError> {
        let block = self
            .blocks
            .get(&reference.block_id)
            .ok_or(ContextError::UnknownBlock(reference))?;
        if block.revision != reference.revision {
            return Err(ContextError::UnknownBlock(reference));
        }
        Ok(block)
    }

    pub fn content(&self, hash: ContentHash) -> Result<&[u8], ContextError> {
        self.content
            .get(&hash)
            .map(Vec::as_slice)
            .ok_or(ContextError::UnknownContent(hash))
    }

    pub fn latest(&self, namespace: &str) -> Option<ContextRef> {
        self.namespaces
            .get(namespace)
            .and_then(|versions| versions.last().copied())
    }

    pub fn subscribe(
        &mut self,
        agent_id: AgentId,
        namespace_pattern: impl Into<String>,
    ) -> Result<SubscriptionId, ContextError> {
        let namespace_pattern = namespace_pattern.into();
        if namespace_pattern.trim().is_empty() {
            return Err(ContextError::InvalidSubscriptionPattern);
        }
        let id = SubscriptionId::new();
        self.subscriptions.insert(
            id,
            ContextSubscription {
                id,
                agent_id,
                namespace_pattern,
            },
        );
        Ok(id)
    }

    pub fn unsubscribe(&mut self, subscription_id: SubscriptionId) -> bool {
        self.subscriptions.remove(&subscription_id).is_some()
    }

    pub fn invalidations(&self) -> &[Invalidation] {
        &self.invalidations
    }

    pub fn invalidate(
        &mut self,
        reference: ContextRef,
        reason: impl Into<String>,
    ) -> Result<Vec<Invalidation>, ContextError> {
        self.block(reference)?;
        let reason = reason.into();
        if let Some(block) = self.blocks.get_mut(&reference.block_id) {
            block.lifecycle = ContextLifecycle::Invalidated;
        }
        let mut invalidations = vec![Invalidation {
            block: reference,
            dependency: None,
            reason: reason.clone(),
        }];
        invalidations.extend(self.mark_dependents_stale(reference, reason));
        self.invalidations.extend(invalidations.iter().cloned());
        Ok(invalidations)
    }

    /// Mark an active or stale block archived while retaining its immutable
    /// content and reference for explicit recovery.
    pub fn archive(&mut self, reference: ContextRef) -> Result<ContextTransition, ContextError> {
        let block = self
            .blocks
            .get_mut(&reference.block_id)
            .ok_or(ContextError::UnknownBlock(reference))?;
        if block.revision != reference.revision {
            return Err(ContextError::UnknownBlock(reference));
        }
        if !matches!(
            block.lifecycle,
            ContextLifecycle::Active | ContextLifecycle::Stale
        ) {
            return Err(ContextError::InvalidLifecycle {
                block: reference,
                operation: "archive",
            });
        }
        block.lifecycle = ContextLifecycle::Archived;
        Ok(ContextTransition::Archived(reference))
    }

    /// Restore an archived block to the active projection set.
    pub fn restore(&mut self, reference: ContextRef) -> Result<ContextTransition, ContextError> {
        let block = self
            .blocks
            .get_mut(&reference.block_id)
            .ok_or(ContextError::UnknownBlock(reference))?;
        if block.revision != reference.revision {
            return Err(ContextError::UnknownBlock(reference));
        }
        if block.lifecycle != ContextLifecycle::Archived {
            return Err(ContextError::InvalidLifecycle {
                block: reference,
                operation: "restore",
            });
        }
        block.lifecycle = ContextLifecycle::Active;
        Ok(ContextTransition::Restored(reference))
    }

    /// Pin an active or stale block for explicit retention policy decisions.
    /// Pinning never changes lifecycle or prompt projection by itself.
    pub fn pin(&mut self, reference: ContextRef) -> Result<ContextTransition, ContextError> {
        let block = self
            .blocks
            .get_mut(&reference.block_id)
            .ok_or(ContextError::UnknownBlock(reference))?;
        if block.revision != reference.revision {
            return Err(ContextError::UnknownBlock(reference));
        }
        if !matches!(
            block.lifecycle,
            ContextLifecycle::Active | ContextLifecycle::Stale
        ) {
            return Err(ContextError::InvalidLifecycle {
                block: reference,
                operation: "pin",
            });
        }
        if block.pinned {
            return Err(ContextError::InvalidLifecycle {
                block: reference,
                operation: "pin",
            });
        }
        block.pinned = true;
        Ok(ContextTransition::Pinned(reference))
    }

    /// Remove an explicit retention pin without changing lifecycle.
    pub fn unpin(&mut self, reference: ContextRef) -> Result<ContextTransition, ContextError> {
        let block = self
            .blocks
            .get_mut(&reference.block_id)
            .ok_or(ContextError::UnknownBlock(reference))?;
        if block.revision != reference.revision {
            return Err(ContextError::UnknownBlock(reference));
        }
        if !block.pinned {
            return Err(ContextError::InvalidLifecycle {
                block: reference,
                operation: "unpin",
            });
        }
        block.pinned = false;
        Ok(ContextTransition::Unpinned(reference))
    }

    pub fn project(
        &self,
        principal: ContextPrincipal,
        request: &ProjectionRequest,
    ) -> ContextProjection {
        let mut blocks = Vec::new();
        let mut estimated_tokens: u64 = 0;
        let mut truncated = false;

        for (namespace, versions) in &self.namespaces {
            if !request
                .namespace_patterns
                .iter()
                .any(|pattern| matches_namespace(pattern, namespace))
            {
                continue;
            }
            let Some(reference) = versions.last() else {
                continue;
            };
            let block = &self.blocks[&reference.block_id];
            if !can_view(principal, block)
                || (!request.include_stale && block.lifecycle != ContextLifecycle::Active)
                || matches!(
                    block.lifecycle,
                    ContextLifecycle::Superseded
                        | ContextLifecycle::Archived
                        | ContextLifecycle::Invalidated
                )
                || !request.trust_policy.allows(block.trust)
            {
                continue;
            }
            if request
                .max_blocks
                .is_some_and(|limit| blocks.len() >= limit)
                || request.max_tokens.is_some_and(|limit| {
                    estimated_tokens.saturating_add(block.token_estimate) > limit
                })
            {
                truncated = true;
                continue;
            }
            estimated_tokens = estimated_tokens.saturating_add(block.token_estimate);
            blocks.push(ProjectedContextBlock {
                block: block.clone(),
            });
        }

        ContextProjection {
            blocks,
            estimated_tokens,
            truncated,
        }
    }

    pub fn render_prompt(
        &self,
        principal: ContextPrincipal,
        layers: &[PromptLayer],
        task: &str,
    ) -> Result<RenderedPrompt, ContextError> {
        self.render_prompt_with_trust_policy(principal, layers, task, ContextTrustPolicy::AllowAll)
    }

    pub fn render_prompt_with_trust_policy(
        &self,
        principal: ContextPrincipal,
        layers: &[PromptLayer],
        task: &str,
        trust_policy: ContextTrustPolicy,
    ) -> Result<RenderedPrompt, ContextError> {
        let mut text = String::new();
        let mut stable_prefix = Vec::new();
        let mut stable = true;

        for layer in layers {
            let mut rendered = format!("[{}]\n", layer.name);
            for reference in &layer.references {
                let block = self.block(*reference)?;
                if !can_view(principal, block) {
                    return Err(ContextError::NotVisible(*reference));
                }
                if !trust_policy.allows(block.trust) {
                    return Err(ContextError::NotTrusted(*reference));
                }
                let content = self.content(block.content_hash)?;
                rendered.push_str(&String::from_utf8_lossy(content));
                rendered.push('\n');
            }
            text.push_str(&rendered);
            if stable && layer.stable {
                stable_prefix.extend_from_slice(rendered.as_bytes());
            } else {
                stable = false;
            }
        }

        text.push_str("[task]\n");
        text.push_str(task);
        text.push('\n');
        let stable_prefix_hash = DeterministicContentHasher.hash(&stable_prefix);
        Ok(RenderedPrompt {
            estimated_tokens: estimate_tokens(text.as_bytes()),
            text,
            stable_prefix_bytes: stable_prefix.len(),
            stable_prefix_hash,
        })
    }

    fn mark_dependents_stale(&mut self, cause: ContextRef, reason: String) -> Vec<Invalidation> {
        let mut queue = vec![cause];
        let mut seen = BTreeSet::new();
        let mut invalidations = Vec::new();
        while let Some(dependency) = queue.pop() {
            if !seen.insert(dependency) {
                continue;
            }
            let dependents = self
                .blocks
                .values()
                .filter(|block| block.dependencies.contains(&dependency))
                .map(ContextBlock::reference)
                .collect::<Vec<_>>();
            for dependent in dependents {
                if let Some(block) = self.blocks.get_mut(&dependent.block_id)
                    && matches!(
                        block.lifecycle,
                        ContextLifecycle::Active | ContextLifecycle::Stale
                    )
                {
                    block.lifecycle = ContextLifecycle::Stale;
                    invalidations.push(Invalidation {
                        block: dependent,
                        dependency: Some(dependency),
                        reason: reason.clone(),
                    });
                    queue.push(dependent);
                }
            }
        }
        invalidations
    }

    fn apply_transition_with_content<F>(
        &mut self,
        transition: ContextTransition,
        resolver: &mut F,
    ) -> Result<(), ContextError>
    where
        F: FnMut([u8; 32]) -> Result<Vec<u8>, ContextError>,
    {
        match transition {
            ContextTransition::Created { block, content } => {
                self.apply_created_block(block, content)?;
            }
            ContextTransition::CreatedFromArtifact {
                block,
                artifact_hash,
            } => {
                let content = resolver(artifact_hash)?;
                self.apply_created_block(block, content)?;
            }
            ContextTransition::Superseded(reference) => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                block.lifecycle = ContextLifecycle::Superseded;
            }
            ContextTransition::Archived(reference) => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                if !matches!(
                    block.lifecycle,
                    ContextLifecycle::Active | ContextLifecycle::Stale
                ) {
                    return Err(ContextError::InvalidLifecycle {
                        block: reference,
                        operation: "archive",
                    });
                }
                block.lifecycle = ContextLifecycle::Archived;
            }
            ContextTransition::Restored(reference) => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                if block.lifecycle != ContextLifecycle::Archived {
                    return Err(ContextError::InvalidLifecycle {
                        block: reference,
                        operation: "restore",
                    });
                }
                block.lifecycle = ContextLifecycle::Active;
            }
            ContextTransition::Pinned(reference) => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                if !matches!(
                    block.lifecycle,
                    ContextLifecycle::Active | ContextLifecycle::Stale
                ) || block.pinned
                {
                    return Err(ContextError::InvalidLifecycle {
                        block: reference,
                        operation: "pin",
                    });
                }
                block.pinned = true;
            }
            ContextTransition::Unpinned(reference) => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                if !block.pinned {
                    return Err(ContextError::InvalidLifecycle {
                        block: reference,
                        operation: "unpin",
                    });
                }
                block.pinned = false;
            }
            ContextTransition::Invalidated {
                block: reference,
                dependency: Some(_),
                ..
            } => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                block.lifecycle = ContextLifecycle::Stale;
            }
            ContextTransition::Invalidated {
                block: reference,
                dependency: None,
                ..
            } => {
                let block = self
                    .blocks
                    .get_mut(&reference.block_id)
                    .ok_or(ContextError::UnknownBlock(reference))?;
                if block.revision != reference.revision {
                    return Err(ContextError::UnknownBlock(reference));
                }
                block.lifecycle = ContextLifecycle::Invalidated;
            }
        }
        Ok(())
    }

    fn apply_created_block(
        &mut self,
        block: ContextBlock,
        content: Vec<u8>,
    ) -> Result<(), ContextError> {
        if self.blocks.contains_key(&block.id) {
            return Err(ContextError::HashCollision(block.content_hash));
        }
        if DeterministicContentHasher.hash(&content) != block.content_hash {
            return Err(ContextError::HashCollision(block.content_hash));
        }
        for dependency in block.dependencies.iter().chain(block.sources.iter()) {
            if !self.blocks.contains_key(&dependency.block_id) {
                return Err(ContextError::UnknownBlock(*dependency));
            }
            let dependency_trust = self.block(*dependency)?.trust;
            if block.trust.combine(dependency_trust) != block.trust {
                return Err(ContextError::TrustUpgrade {
                    block: block.reference(),
                    dependency: *dependency,
                });
            }
        }
        if let Some(existing) = self.content.get(&block.content_hash) {
            if existing != &content {
                return Err(ContextError::HashCollision(block.content_hash));
            }
        } else {
            self.content.insert(block.content_hash, content);
        }
        let reference = block.reference();
        self.namespaces
            .entry(block.namespace.clone())
            .or_default()
            .push(reference);
        self.blocks.insert(block.id, block);
        Ok(())
    }
}

pub fn encode_transition(transition: &ContextTransition) -> Result<Vec<u8>, ContextError> {
    let mut encoder = ContextEncoder::default();
    match transition {
        ContextTransition::Created { block, content } => {
            encoder.u8(0);
            encode_block(&mut encoder, block)?;
            encoder.bytes(content)?;
        }
        ContextTransition::CreatedFromArtifact {
            block,
            artifact_hash,
        } => {
            encoder.u8(3);
            encode_block(&mut encoder, block)?;
            encoder.array(artifact_hash);
        }
        ContextTransition::Superseded(reference) => {
            encoder.u8(1);
            encode_reference(&mut encoder, *reference);
        }
        ContextTransition::Archived(reference) => {
            encoder.u8(4);
            encode_reference(&mut encoder, *reference);
        }
        ContextTransition::Restored(reference) => {
            encoder.u8(5);
            encode_reference(&mut encoder, *reference);
        }
        ContextTransition::Pinned(reference) => {
            encoder.u8(6);
            encode_reference(&mut encoder, *reference);
        }
        ContextTransition::Unpinned(reference) => {
            encoder.u8(7);
            encode_reference(&mut encoder, *reference);
        }
        ContextTransition::Invalidated {
            block,
            dependency,
            reason,
        } => {
            encoder.u8(2);
            encode_reference(&mut encoder, *block);
            match dependency {
                Some(reference) => {
                    encoder.u8(1);
                    encode_reference(&mut encoder, *reference);
                }
                None => encoder.u8(0),
            }
            encoder.string(reason)?;
        }
    }
    Ok(encoder.finish())
}

pub fn decode_transition(version: u16, bytes: &[u8]) -> Result<ContextTransition, ContextError> {
    if version != CONTEXT_TRANSITION_VERSION {
        return Err(ContextError::InvalidEncoding(format!(
            "unsupported context transition version {version}"
        )));
    }
    if bytes.len() > MAX_CONTEXT_PAYLOAD_BYTES {
        return Err(ContextError::InvalidEncoding(
            "context transition exceeds maximum payload size".to_string(),
        ));
    }
    let mut decoder = ContextDecoder::new(bytes);
    let transition = match decoder.u8()? {
        0 => ContextTransition::Created {
            block: decode_block(&mut decoder)?,
            content: decoder.bytes()?,
        },
        3 => ContextTransition::CreatedFromArtifact {
            block: decode_block(&mut decoder)?,
            artifact_hash: decoder.array()?,
        },
        1 => ContextTransition::Superseded(decode_reference(&mut decoder)?),
        4 => ContextTransition::Archived(decode_reference(&mut decoder)?),
        5 => ContextTransition::Restored(decode_reference(&mut decoder)?),
        6 => ContextTransition::Pinned(decode_reference(&mut decoder)?),
        7 => ContextTransition::Unpinned(decode_reference(&mut decoder)?),
        2 => {
            let block = decode_reference(&mut decoder)?;
            let dependency = match decoder.u8()? {
                0 => None,
                1 => Some(decode_reference(&mut decoder)?),
                tag => {
                    return Err(ContextError::InvalidEncoding(format!(
                        "unknown dependency presence tag {tag}"
                    )));
                }
            };
            ContextTransition::Invalidated {
                block,
                dependency,
                reason: decoder.string()?,
            }
        }
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context transition tag {tag}"
            )));
        }
    };
    decoder.finish()?;
    Ok(transition)
}

#[derive(Default)]
struct ContextEncoder {
    bytes: Vec<u8>,
}

impl ContextEncoder {
    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn array(&mut self, value: &[u8; 32]) {
        self.bytes.extend_from_slice(value);
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), ContextError> {
        let length = u32::try_from(value.len()).map_err(|_| {
            ContextError::InvalidEncoding("context value exceeds u32::MAX".to_string())
        })?;
        self.u64(u64::from(length));
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn string(&mut self, value: &str) -> Result<(), ContextError> {
        self.bytes(value.as_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct ContextDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ContextDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ContextError> {
        let end = self.offset.checked_add(length).ok_or_else(|| {
            ContextError::InvalidEncoding("context decode offset overflow".to_string())
        })?;
        if end > self.bytes.len() {
            return Err(ContextError::InvalidEncoding(
                "context payload is truncated".to_string(),
            ));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ContextError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, ContextError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("width checked"),
        ))
    }

    fn array(&mut self) -> Result<[u8; 32], ContextError> {
        Ok(self.take(32)?.try_into().expect("width checked"))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, ContextError> {
        let length = usize::try_from(self.u64()?).map_err(|_| {
            ContextError::InvalidEncoding("context value length overflows usize".to_string())
        })?;
        if length > MAX_CONTEXT_PAYLOAD_BYTES {
            return Err(ContextError::InvalidEncoding(
                "context value exceeds maximum payload size".to_string(),
            ));
        }
        Ok(self.take(length)?.to_vec())
    }

    fn string(&mut self) -> Result<String, ContextError> {
        String::from_utf8(self.bytes()?)
            .map_err(|_| ContextError::InvalidEncoding("context string is not UTF-8".to_string()))
    }

    fn count(&mut self) -> Result<usize, ContextError> {
        let count = usize::try_from(self.u64()?).map_err(|_| {
            ContextError::InvalidEncoding("context item count overflows usize".to_string())
        })?;
        if count > MAX_CONTEXT_ITEMS {
            return Err(ContextError::InvalidEncoding(
                "context item count exceeds limit".to_string(),
            ));
        }
        Ok(count)
    }

    fn finish(&self) -> Result<(), ContextError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(ContextError::InvalidEncoding(
                "context payload has trailing bytes".to_string(),
            ))
        }
    }
}

fn encode_reference(encoder: &mut ContextEncoder, reference: ContextRef) {
    encoder.u64(reference.block_id.value());
    encoder.u64(reference.revision);
}

fn decode_reference(decoder: &mut ContextDecoder<'_>) -> Result<ContextRef, ContextError> {
    Ok(ContextRef {
        block_id: ContextBlockId::from_u64(decoder.u64()?),
        revision: decoder.u64()?,
    })
}

fn encode_block(encoder: &mut ContextEncoder, block: &ContextBlock) -> Result<(), ContextError> {
    encoder.u64(block.id.value());
    encoder.u64(block.revision);
    encoder.string(&block.namespace)?;
    encode_kind(encoder, &block.kind)?;
    encode_owner(encoder, block.owner);
    encode_scope(encoder, block.scope);
    encoder.array(&block.content_hash.as_bytes());
    encoder.u64(block.token_estimate);
    encoder.u8(trust_tag(block.trust));
    encoder.u8(block.importance);
    encoder.u8(lifecycle_tag(block.lifecycle));
    encode_references(encoder, &block.dependencies)?;
    encode_references(encoder, &block.sources)?;
    encode_optional_u64(encoder, block.created_event.map(EventId::value));
    encode_optional_u64(encoder, block.last_access_ms);
    Ok(())
}

fn decode_block(decoder: &mut ContextDecoder<'_>) -> Result<ContextBlock, ContextError> {
    let id = ContextBlockId::from_u64(decoder.u64()?);
    let revision = decoder.u64()?;
    let namespace = decoder.string()?;
    let kind = decode_kind(decoder)?;
    let owner = decode_owner(decoder)?;
    let scope = decode_scope(decoder)?;
    let content_hash = ContentHash::from_digest(decoder.array()?);
    let token_estimate = decoder.u64()?;
    let trust = decode_trust(decoder.u8()?)?;
    let importance = decoder.u8()?;
    let lifecycle = decode_lifecycle(decoder.u8()?)?;
    let dependencies = decode_references(decoder)?;
    let sources = decode_references(decoder)?;
    let created_event = decode_optional_u64(decoder)?.map(EventId::from_u64);
    let last_access_ms = decode_optional_u64(decoder)?;
    Ok(ContextBlock {
        id,
        revision,
        namespace,
        kind,
        owner,
        scope,
        content_hash,
        token_estimate,
        trust,
        importance,
        lifecycle,
        pinned: false,
        dependencies,
        sources,
        created_event,
        last_access_ms,
    })
}

fn encode_kind(encoder: &mut ContextEncoder, kind: &ContextKind) -> Result<(), ContextError> {
    match kind {
        ContextKind::Note => encoder.u8(0),
        ContextKind::Project => encoder.u8(1),
        ContextKind::Contract => encoder.u8(2),
        ContextKind::Task => encoder.u8(3),
        ContextKind::Result => encoder.u8(4),
        ContextKind::ArtifactReference => encoder.u8(5),
        ContextKind::Custom(value) => {
            encoder.u8(6);
            encoder.string(value)?;
        }
    }
    Ok(())
}

fn decode_kind(decoder: &mut ContextDecoder<'_>) -> Result<ContextKind, ContextError> {
    Ok(match decoder.u8()? {
        0 => ContextKind::Note,
        1 => ContextKind::Project,
        2 => ContextKind::Contract,
        3 => ContextKind::Task,
        4 => ContextKind::Result,
        5 => ContextKind::ArtifactReference,
        6 => ContextKind::Custom(decoder.string()?),
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context kind tag {tag}"
            )));
        }
    })
}

fn encode_owner(encoder: &mut ContextEncoder, owner: ContextOwner) {
    match owner {
        ContextOwner::Runtime => encoder.u8(0),
        ContextOwner::Agent(agent_id) => {
            encoder.u8(1);
            encoder.u64(agent_id.value());
        }
    }
}

fn decode_owner(decoder: &mut ContextDecoder<'_>) -> Result<ContextOwner, ContextError> {
    Ok(match decoder.u8()? {
        0 => ContextOwner::Runtime,
        1 => ContextOwner::Agent(AgentId::from_u64(decoder.u64()?)),
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context owner tag {tag}"
            )));
        }
    })
}

fn encode_scope(encoder: &mut ContextEncoder, scope: ContextScope) {
    match scope {
        ContextScope::Global => encoder.u8(0),
        ContextScope::Team => encoder.u8(1),
        ContextScope::Private(agent_id) => {
            encoder.u8(2);
            encoder.u64(agent_id.value());
        }
    }
}

fn decode_scope(decoder: &mut ContextDecoder<'_>) -> Result<ContextScope, ContextError> {
    Ok(match decoder.u8()? {
        0 => ContextScope::Global,
        1 => ContextScope::Team,
        2 => ContextScope::Private(AgentId::from_u64(decoder.u64()?)),
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context scope tag {tag}"
            )));
        }
    })
}

fn trust_tag(trust: TrustLevel) -> u8 {
    match trust {
        TrustLevel::TrustedProject => 0,
        TrustLevel::UserProvided => 1,
        TrustLevel::Generated => 2,
        TrustLevel::RemoteAgent => 3,
        TrustLevel::WebUntrusted => 4,
        TrustLevel::McpMetadata => 5,
        TrustLevel::McpResult => 6,
        TrustLevel::External => 7,
        TrustLevel::Runtime => 8,
    }
}

fn decode_trust(tag: u8) -> Result<TrustLevel, ContextError> {
    Ok(match tag {
        0 => TrustLevel::TrustedProject,
        1 => TrustLevel::UserProvided,
        2 => TrustLevel::Generated,
        3 => TrustLevel::RemoteAgent,
        4 => TrustLevel::WebUntrusted,
        5 => TrustLevel::McpMetadata,
        6 => TrustLevel::McpResult,
        7 => TrustLevel::External,
        8 => TrustLevel::Runtime,
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context trust tag {tag}"
            )));
        }
    })
}

fn lifecycle_tag(lifecycle: ContextLifecycle) -> u8 {
    match lifecycle {
        ContextLifecycle::Active => 0,
        ContextLifecycle::Stale => 1,
        ContextLifecycle::Superseded => 2,
        ContextLifecycle::Archived => 3,
        ContextLifecycle::Invalidated => 4,
    }
}

fn decode_lifecycle(tag: u8) -> Result<ContextLifecycle, ContextError> {
    Ok(match tag {
        0 => ContextLifecycle::Active,
        1 => ContextLifecycle::Stale,
        2 => ContextLifecycle::Superseded,
        3 => ContextLifecycle::Archived,
        4 => ContextLifecycle::Invalidated,
        tag => {
            return Err(ContextError::InvalidEncoding(format!(
                "unknown context lifecycle tag {tag}"
            )));
        }
    })
}

fn encode_references(
    encoder: &mut ContextEncoder,
    references: &[ContextRef],
) -> Result<(), ContextError> {
    let count = u64::try_from(references.len()).map_err(|_| {
        ContextError::InvalidEncoding("context reference count exceeds u64::MAX".to_string())
    })?;
    encoder.u64(count);
    for reference in references {
        encode_reference(encoder, *reference);
    }
    Ok(())
}

fn decode_references(decoder: &mut ContextDecoder<'_>) -> Result<Vec<ContextRef>, ContextError> {
    let count = decoder.count()?;
    (0..count).map(|_| decode_reference(decoder)).collect()
}

fn encode_optional_u64(encoder: &mut ContextEncoder, value: Option<u64>) {
    match value {
        Some(value) => {
            encoder.u8(1);
            encoder.u64(value);
        }
        None => encoder.u8(0),
    }
}

fn decode_optional_u64(decoder: &mut ContextDecoder<'_>) -> Result<Option<u64>, ContextError> {
    match decoder.u8()? {
        0 => Ok(None),
        1 => Ok(Some(decoder.u64()?)),
        tag => Err(ContextError::InvalidEncoding(format!(
            "unknown optional value tag {tag}"
        ))),
    }
}

fn can_view(principal: ContextPrincipal, block: &ContextBlock) -> bool {
    match block.scope {
        ContextScope::Global | ContextScope::Team => true,
        ContextScope::Private(owner) => {
            principal == ContextPrincipal::Runtime || principal == ContextPrincipal::Agent(owner)
        }
    }
}

fn matches_namespace(pattern: &str, namespace: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return namespace.starts_with(prefix)
            && namespace.as_bytes().get(prefix.len()) == Some(&b'.');
    }
    pattern == namespace
}

fn estimate_tokens(bytes: &[u8]) -> u64 {
    bytes.len().div_ceil(4) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> AgentId {
        AgentId::new()
    }

    fn draft(
        namespace: &str,
        owner: ContextOwner,
        scope: ContextScope,
        content: &str,
    ) -> ContextDraft {
        ContextDraft::new(
            namespace,
            ContextKind::Contract,
            owner,
            scope,
            content.as_bytes().to_vec(),
        )
    }

    #[test]
    fn proprioception_reports_freshness_pressure_without_mutation() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let schema = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: UUID",
            ))
            .expect("schema should publish");
        graph
            .publish(
                draft(
                    "auth.contract",
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    "JWT subject uses users.id",
                )
                .with_dependency(schema.block.reference()),
            )
            .expect("dependent should publish");
        graph
            .publish(
                draft(
                    "project.rules",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "stable project rules",
                )
                .with_token_estimate(40),
            )
            .expect("project rules should publish");
        let replacement = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: BIGINT",
            ))
            .expect("replacement should publish");
        graph
            .invalidate(replacement.block.reference(), "schema contract changed")
            .expect("replacement should invalidate");

        let dashboard = graph.proprioception(ContextFreshnessPolicy {
            max_active_tokens: Some(10),
            max_stale_blocks: Some(0),
            largest_block_limit: 2,
            recent_invalidation_limit: 1,
        });
        assert_eq!(dashboard.active_blocks, 1);
        assert_eq!(dashboard.active_tokens, 40);
        assert_eq!(dashboard.stale_blocks, 1);
        assert_eq!(dashboard.invalidated_blocks, 1);
        assert!(dashboard.active_tokens_over_budget);
        assert!(dashboard.stale_blocks_over_budget);
        assert_eq!(dashboard.largest_blocks.len(), 2);
        assert_eq!(dashboard.largest_blocks[0].namespace, "project.rules");
        assert_eq!(dashboard.recent_invalidations.len(), 1);
        assert_eq!(
            dashboard.recent_invalidations[0].reason,
            "schema contract changed"
        );
        assert_eq!(
            graph
                .block(replacement.block.reference())
                .expect("replacement should remain available")
                .lifecycle,
            ContextLifecycle::Invalidated
        );
    }

    #[test]
    fn bounded_search_and_dependency_reports_are_read_only_and_visibility_scoped() {
        let owner = agent();
        let other = agent();
        let mut graph = ContextGraph::new();
        let schema = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: UUID",
            ))
            .expect("schema should publish");
        let dependent = graph
            .publish(
                draft(
                    "auth.contract",
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    "JWT subject uses users.id",
                )
                .with_dependency(schema.block.reference()),
            )
            .expect("dependent should publish");
        let private = graph
            .publish(draft(
                "private.secret",
                ContextOwner::Agent(owner),
                ContextScope::Private(owner),
                "owner-only token",
            ))
            .expect("private block should publish");

        let hits = graph
            .search(
                ContextPrincipal::Agent(other),
                &ContextSearchRequest::new("jwt"),
            )
            .expect("search should succeed");
        assert!(hits.is_empty());
        let owner_hits = graph
            .search(
                ContextPrincipal::Agent(owner),
                &ContextSearchRequest::new("jwt"),
            )
            .expect("owner search should succeed");
        assert_eq!(owner_hits.len(), 1);
        assert_eq!(owner_hits[0].summary.reference, dependent.block.reference());
        assert!(owner_hits[0].matched_content);

        let secret_hits = graph
            .search(
                ContextPrincipal::Agent(owner),
                &ContextSearchRequest::new("secret"),
            )
            .expect("private search should succeed");
        assert_eq!(secret_hits[0].summary.reference, private.block.reference());
        let report = graph
            .dependency_report(schema.block.reference(), 1)
            .expect("dependency report should succeed");
        assert_eq!(report.dependencies, Vec::<ContextRef>::new());
        assert_eq!(report.sources, Vec::<ContextRef>::new());
        assert_eq!(report.dependents, vec![dependent.block.reference()]);
        assert!(!report.dependents_truncated);
        assert!(matches!(
            graph.search(ContextPrincipal::Runtime, &ContextSearchRequest::new(" ")),
            Err(ContextError::InvalidSearchQuery(_))
        ));
    }

    #[test]
    fn archive_and_restore_are_validated_replayable_lifecycle_transitions() {
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(draft(
                "project.rules",
                ContextOwner::Runtime,
                ContextScope::Global,
                "stable project rules",
            ))
            .expect("context should publish");
        let created = publication.transitions[0].clone();
        let reference = publication.block.reference();
        let archived = graph
            .archive(reference)
            .expect("active block should archive");
        assert_eq!(
            graph
                .block(reference)
                .expect("block should remain recoverable")
                .lifecycle,
            ContextLifecycle::Archived
        );
        assert!(matches!(
            graph.archive(reference),
            Err(ContextError::InvalidLifecycle {
                operation: "archive",
                ..
            })
        ));
        let restored = graph
            .restore(reference)
            .expect("archived block should restore");
        assert_eq!(
            graph
                .block(reference)
                .expect("block should remain present")
                .lifecycle,
            ContextLifecycle::Active
        );
        assert!(matches!(
            graph.restore(reference),
            Err(ContextError::InvalidLifecycle {
                operation: "restore",
                ..
            })
        ));

        let archived_bytes = encode_transition(&archived).expect("archive should encode");
        let restored_bytes = encode_transition(&restored).expect("restore should encode");
        assert_eq!(
            decode_transition(CONTEXT_TRANSITION_VERSION, &archived_bytes)
                .expect("archive should decode"),
            archived
        );
        assert_eq!(
            decode_transition(CONTEXT_TRANSITION_VERSION, &restored_bytes)
                .expect("restore should decode"),
            restored
        );
        let replayed = ContextGraph::replay([created, archived, restored])
            .expect("lifecycle transitions should replay");
        assert_eq!(
            replayed.block(reference).expect("replayed block").lifecycle,
            ContextLifecycle::Active
        );
    }

    #[test]
    fn pinning_is_explicit_replayable_and_does_not_change_lifecycle() {
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(draft(
                "project.rules",
                ContextOwner::Runtime,
                ContextScope::Global,
                "retain this",
            ))
            .expect("context should publish");
        let created = publication.transitions[0].clone();
        let reference = publication.block.reference();
        let pinned = graph.pin(reference).expect("active block should pin");
        assert_eq!(
            graph.block(reference).expect("pinned block").lifecycle,
            ContextLifecycle::Active
        );
        assert!(graph.block(reference).expect("pinned block").pinned);
        assert!(matches!(
            graph.pin(reference),
            Err(ContextError::InvalidLifecycle {
                operation: "pin",
                ..
            })
        ));
        let dashboard = graph.proprioception(ContextFreshnessPolicy::default());
        assert_eq!(dashboard.pinned_blocks, 1);
        assert!(dashboard.largest_blocks[0].pinned);

        let unpinned = graph.unpin(reference).expect("pinned block should unpin");
        assert!(!graph.block(reference).expect("unpinned block").pinned);
        assert!(matches!(
            graph.unpin(reference),
            Err(ContextError::InvalidLifecycle {
                operation: "unpin",
                ..
            })
        ));
        let pinned_bytes = encode_transition(&pinned).expect("pin should encode");
        let unpinned_bytes = encode_transition(&unpinned).expect("unpin should encode");
        assert_eq!(
            decode_transition(CONTEXT_TRANSITION_VERSION, &pinned_bytes)
                .expect("pin should decode"),
            pinned
        );
        assert_eq!(
            decode_transition(CONTEXT_TRANSITION_VERSION, &unpinned_bytes)
                .expect("unpin should decode"),
            unpinned
        );
        let replayed =
            ContextGraph::replay([created, pinned, unpinned]).expect("pin lifecycle should replay");
        assert!(!replayed.block(reference).expect("replayed block").pinned);
    }

    #[test]
    fn publishing_versions_marks_dependents_stale() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let schema = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: UUID",
            ))
            .expect("schema should publish");
        let dependent = graph
            .publish(
                draft(
                    "auth.contract",
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    "JWT subject uses users.id",
                )
                .with_dependency(schema.block.reference()),
            )
            .expect("dependent should publish");

        let replacement = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: BIGINT",
            ))
            .expect("replacement should publish");

        assert_eq!(
            graph
                .block(schema.block.reference())
                .expect("old block")
                .lifecycle,
            ContextLifecycle::Superseded
        );
        assert_eq!(
            graph
                .block(dependent.block.reference())
                .expect("dependent")
                .lifecycle,
            ContextLifecycle::Stale
        );
        assert_eq!(replacement.invalidations.len(), 1);
        assert_eq!(
            replacement.invalidations[0].block,
            dependent.block.reference()
        );
    }

    #[test]
    fn context_transitions_replay_into_the_same_authoritative_state() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let first = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: UUID",
            ))
            .expect("first block should publish");
        let dependent = graph
            .publish(
                draft(
                    "auth.contract",
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    "JWT subject uses users.id",
                )
                .with_dependency(first.block.reference()),
            )
            .expect("dependent should publish");
        let replacement = graph
            .publish(draft(
                "schema.users",
                ContextOwner::Agent(owner),
                ContextScope::Team,
                "id: BIGINT",
            ))
            .expect("replacement should publish");

        let mut log = ContextEventLog::new();
        log.record(&first);
        log.record(&dependent);
        log.record(&replacement);
        let events = log
            .to_events(RunId::new())
            .expect("context transitions should encode");
        let persisted_log =
            ContextEventLog::from_events(&events).expect("context transition events should decode");
        assert_eq!(persisted_log, log);
        let replayed = persisted_log
            .replay()
            .expect("context transitions should replay");

        assert_eq!(
            replayed.latest("schema.users"),
            graph.latest("schema.users")
        );
        assert_eq!(
            replayed
                .block(dependent.block.reference())
                .expect("replayed dependent")
                .lifecycle,
            ContextLifecycle::Stale
        );
        assert_eq!(
            replayed
                .content(replacement.block.content_hash)
                .expect("replayed content"),
            b"id: BIGINT"
        );
    }

    #[test]
    fn context_codec_rejects_unknown_versions_and_trailing_bytes() {
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(draft(
                "project.rules",
                ContextOwner::Runtime,
                ContextScope::Global,
                "typed state",
            ))
            .expect("context should publish");
        let transition = publication.transitions.first().expect("created transition");
        let mut bytes = encode_transition(transition).expect("transition should encode");
        bytes.push(0);

        assert!(matches!(
            decode_transition(CONTEXT_TRANSITION_VERSION + 1, &bytes),
            Err(ContextError::InvalidEncoding(_))
        ));
        assert!(matches!(
            decode_transition(CONTEXT_TRANSITION_VERSION, &bytes),
            Err(ContextError::InvalidEncoding(_))
        ));
    }

    #[test]
    fn artifact_backed_context_replay_resolves_and_verifies_content() {
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(draft(
                "project.large",
                ContextOwner::Runtime,
                ContextScope::Global,
                "large context payload",
            ))
            .expect("context should publish");
        let block = publication.block;
        let transition = ContextTransition::CreatedFromArtifact {
            block: block.clone(),
            artifact_hash: [7; 32],
        };

        let recovered = ContextGraph::replay_with_content([transition.clone()], |artifact_hash| {
            assert_eq!(artifact_hash, [7; 32]);
            Ok(b"large context payload".to_vec())
        })
        .expect("artifact-backed context should replay");
        assert_eq!(
            recovered.content(block.content_hash).expect("content"),
            b"large context payload"
        );

        assert!(matches!(
            ContextGraph::replay_with_content([transition], |_| Ok(b"tampered".to_vec())),
            Err(ContextError::HashCollision(_))
        ));
    }

    #[test]
    fn private_context_is_not_projected_to_other_agents() {
        let owner = agent();
        let other = agent();
        let mut graph = ContextGraph::new();
        let private = graph
            .publish(draft(
                "agent.secret",
                ContextOwner::Agent(owner),
                ContextScope::Private(owner),
                "private note",
            ))
            .expect("private block should publish");
        let public = graph
            .publish(draft(
                "project.rules",
                ContextOwner::Runtime,
                ContextScope::Global,
                "be explicit",
            ))
            .expect("public block should publish");

        let request = ProjectionRequest::all();
        let owner_projection = graph.project(ContextPrincipal::Agent(owner), &request);
        let other_projection = graph.project(ContextPrincipal::Agent(other), &request);
        assert!(
            owner_projection
                .blocks
                .iter()
                .any(|block| block.block.id == private.block.id)
        );
        assert!(
            owner_projection
                .blocks
                .iter()
                .any(|block| block.block.id == public.block.id)
        );
        assert!(
            other_projection
                .blocks
                .iter()
                .all(|block| block.block.id != private.block.id)
        );
        assert!(
            other_projection
                .blocks
                .iter()
                .any(|block| block.block.id == public.block.id)
        );
    }

    #[test]
    fn subscriptions_awaken_only_matching_visible_agents() {
        let owner = agent();
        let other = agent();
        let mut graph = ContextGraph::new();
        graph
            .subscribe(owner, "schema.users.*")
            .expect("subscription should register");
        graph
            .subscribe(other, "api.*")
            .expect("subscription should register");

        let publication = graph
            .publish(draft(
                "schema.users.v2",
                ContextOwner::Runtime,
                ContextScope::Team,
                "id: UUID",
            ))
            .expect("schema should publish");
        assert!(publication.awakened_agents.contains(&owner));
        assert!(!publication.awakened_agents.contains(&other));
    }

    #[test]
    fn projections_enforce_block_and_token_limits() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        graph
            .publish(
                draft(
                    "project.one",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "one",
                )
                .with_token_estimate(3),
            )
            .expect("first block should publish");
        graph
            .publish(
                draft(
                    "project.two",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "two",
                )
                .with_token_estimate(3),
            )
            .expect("second block should publish");

        let block_limited = graph.project(
            ContextPrincipal::Agent(owner),
            &ProjectionRequest {
                max_blocks: Some(1),
                ..ProjectionRequest::all()
            },
        );
        assert_eq!(block_limited.blocks.len(), 1);
        assert!(block_limited.truncated);

        let token_limited = graph.project(
            ContextPrincipal::Agent(owner),
            &ProjectionRequest {
                max_tokens: Some(2),
                ..ProjectionRequest::all()
            },
        );
        assert!(token_limited.blocks.is_empty());
        assert!(token_limited.truncated);
    }

    #[test]
    fn projections_and_prompts_enforce_trust_policy() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let trusted = graph
            .publish(
                draft(
                    "project.trusted",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "trusted rule",
                )
                .with_trust(TrustLevel::TrustedProject),
            )
            .expect("trusted block should publish");
        let generated = graph
            .publish(draft(
                "project.generated",
                ContextOwner::Runtime,
                ContextScope::Global,
                "generated note",
            ))
            .expect("generated block should publish");
        let external = graph
            .publish(
                draft(
                    "project.external",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "untrusted page",
                )
                .with_trust(TrustLevel::WebUntrusted),
            )
            .expect("external block should publish");

        let trusted_only = graph.project(
            ContextPrincipal::Agent(owner),
            &ProjectionRequest {
                trust_policy: ContextTrustPolicy::TrustedOnly,
                ..ProjectionRequest::all()
            },
        );
        assert_eq!(
            trusted_only
                .blocks
                .iter()
                .map(|block| block.block.id)
                .collect::<Vec<_>>(),
            vec![trusted.block.id]
        );

        let excludes_external = graph.project(
            ContextPrincipal::Agent(owner),
            &ProjectionRequest {
                trust_policy: ContextTrustPolicy::ExcludeExternal,
                ..ProjectionRequest::all()
            },
        );
        assert!(
            excludes_external
                .blocks
                .iter()
                .any(|block| block.block.id == generated.block.id)
        );
        assert!(
            !excludes_external
                .blocks
                .iter()
                .any(|block| block.block.id == external.block.id)
        );

        let layers = vec![PromptLayer::stable(
            "context",
            vec![external.block.reference()],
        )];
        assert!(matches!(
            graph.render_prompt_with_trust_policy(
                ContextPrincipal::Agent(owner),
                &layers,
                "task",
                ContextTrustPolicy::TrustedOnly,
            ),
            Err(ContextError::NotTrusted(reference)) if reference == external.block.reference()
        ));
    }

    #[test]
    fn derived_context_combines_dependency_trust_without_upgrade() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let external = graph
            .publish(
                draft(
                    "source.web",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "untrusted source",
                )
                .with_trust(TrustLevel::WebUntrusted),
            )
            .expect("source should publish");

        let derived = graph
            .publish(
                draft(
                    "result.summary",
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    "summary derived from source",
                )
                .with_trust(TrustLevel::TrustedProject)
                .with_dependency(external.block.reference()),
            )
            .expect("derived block should publish");

        assert_eq!(derived.block.trust, TrustLevel::WebUntrusted);
    }

    #[test]
    fn replay_rejects_a_persisted_context_trust_upgrade() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let external = graph
            .publish(
                draft(
                    "source.web",
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    "untrusted source",
                )
                .with_trust(TrustLevel::WebUntrusted),
            )
            .expect("source should publish");
        let mut forged = draft(
            "result.summary",
            ContextOwner::Agent(owner),
            ContextScope::Private(owner),
            "summary derived from source",
        )
        .with_trust(TrustLevel::TrustedProject)
        .with_dependency(external.block.reference());
        forged.created_event = Some(EventId::new());
        let content_hash = DeterministicContentHasher.hash(&forged.content);
        let block = ContextBlock {
            id: ContextBlockId::new(),
            revision: 1,
            namespace: forged.namespace,
            kind: forged.kind,
            owner: forged.owner,
            scope: forged.scope,
            content_hash,
            token_estimate: estimate_tokens(&forged.content),
            trust: forged.trust,
            importance: forged.importance,
            lifecycle: ContextLifecycle::Active,
            pinned: false,
            dependencies: forged.dependencies,
            sources: forged.sources,
            created_event: forged.created_event,
            last_access_ms: None,
        };

        let result = ContextGraph::replay([
            ContextTransition::Created {
                block: external.block,
                content: b"untrusted source".to_vec(),
            },
            ContextTransition::Created {
                block,
                content: b"summary derived from source".to_vec(),
            },
        ]);
        assert!(matches!(result, Err(ContextError::TrustUpgrade { .. })));
    }

    #[test]
    fn prompt_renderer_preserves_stable_prefix_hash() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let rules = graph
            .publish(draft(
                "project.rules",
                ContextOwner::Runtime,
                ContextScope::Global,
                "Use typed state.",
            ))
            .expect("rules should publish");
        let layers = vec![PromptLayer::stable(
            "runtime",
            vec![rules.block.reference()],
        )];
        let first = graph
            .render_prompt(ContextPrincipal::Agent(owner), &layers, "first task")
            .expect("prompt should render");
        let second = graph
            .render_prompt(ContextPrincipal::Agent(owner), &layers, "second task")
            .expect("prompt should render");
        assert_eq!(first.stable_prefix_hash, second.stable_prefix_hash);
        assert_ne!(first.text, second.text);
        assert!(first.text.contains("Use typed state."));
        assert!(first.stable_prefix_bytes > 0);
    }

    #[test]
    fn content_is_deduplicated_by_hash() {
        let owner = agent();
        let mut graph = ContextGraph::new();
        let first = graph
            .publish(draft(
                "one",
                ContextOwner::Agent(owner),
                ContextScope::Private(owner),
                "same",
            ))
            .expect("first block should publish");
        let second = graph
            .publish(draft(
                "two",
                ContextOwner::Agent(owner),
                ContextScope::Private(owner),
                "same",
            ))
            .expect("second block should publish");
        assert_eq!(first.block.content_hash, second.block.content_hash);
        assert_eq!(
            graph.content(first.block.content_hash).expect("content"),
            b"same"
        );
    }
}
