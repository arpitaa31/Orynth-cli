//! Runtime-service orchestration over authoritative event and context state.
//!
//! This crate intentionally composes the kernel, event store, and context
//! domains. The event store preserves events; the runtime service decides how
//! to hydrate the materialized projections used by callers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use orynth_assumptions::{
    ASSUMPTION_SCHEMA_VERSION, Assumption, AssumptionError, AssumptionGraph, AssumptionPublication,
    encode_transition,
};
use orynth_cache::{CacheError, CacheKey, CacheObservation, CacheTelemetry};
use orynth_context::{
    ContextDependencyReport, ContextError, ContextEventLog, ContextFreshnessPolicy, ContextGraph,
    ContextPrincipal, ContextProprioception, ContextRef, ContextSearchHit, ContextSearchRequest,
    ContextTransition,
};
use orynth_event_store::{
    AgentStatus, ArtifactError, ArtifactStore, ContentHash, EventStore, RuntimeState, StoreError,
    StoredEvent,
};
use orynth_failure_memory::{
    FAILURE_MEMORY_SCHEMA_VERSION, FailureMemory, FailureMemoryError, FailureRecord,
    FailureTransition,
};
use orynth_ipc::{
    BoundedMailbox, DEFAULT_MAILBOX_CAPACITY, IPC_SCHEMA_VERSION, IpcEnvelope, IpcError,
    IpcMessage, IpcProvenance, RUNTIME_AGENT_ID,
};
use orynth_kernel::{
    AgentId, AgentIdentity, AssumptionId, ConflictId, Event, EventKind, FailureId, MessageId,
    ModelRef, RunId, TrustOrigin, Usage,
};
use orynth_scheduler::{
    BudgetLimits, BudgetState, BudgetUsage, CacheAwareRouteCandidate, CacheRoutingPolicy,
    HealthSignal, ModelRouteCandidate, RankedModelRoute, SCHEDULER_SCHEMA_VERSION, SchedulerError,
    SchedulerState, SupervisionAction, SupervisionPolicy, choose_supervision_action,
    rank_cache_aware_candidates, rank_cache_aware_candidates_at,
};
use orynth_security::{
    CAPABILITY_SCHEMA_VERSION, CapabilityError, CapabilityLease, CapabilityPolicy,
    CapabilityTransition, encode_transition as encode_capability_transition,
};
use orynth_specialist::{
    SPECIALIST_SCHEMA_VERSION, SpecialistError, SpecialistProfile, SpecialistRegistry,
    SpecialistSelectionRequest, SpecialistTransition,
};
use orynth_tool_runtime::{
    TOOL_SCHEMA_VERSION, ToolError, ToolHistory, ToolTransition,
    encode_transition as encode_tool_transition,
};

pub const CONTEXT_ARTIFACT_MEDIA_TYPE: &str = "application/vnd.orynth.context-block";

#[derive(Debug)]
pub enum RuntimeError {
    EventStore(StoreError),
    Artifact(ArtifactError),
    Cache(CacheError),
    Context(ContextError),
    Ipc(IpcError),
    Assumption(AssumptionError),
    Scheduler(SchedulerError),
    Specialist(SpecialistError),
    Capability(CapabilityError),
    Tool(ToolError),
    FailureMemory(FailureMemoryError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventStore(error) => write!(formatter, "runtime event-store error: {error}"),
            Self::Artifact(error) => write!(formatter, "runtime artifact-store error: {error}"),
            Self::Cache(error) => write!(formatter, "runtime cache telemetry error: {error}"),
            Self::Context(error) => write!(formatter, "runtime context recovery error: {error}"),
            Self::Ipc(error) => write!(formatter, "runtime IPC error: {error}"),
            Self::Assumption(error) => write!(formatter, "runtime assumption error: {error}"),
            Self::Scheduler(error) => write!(formatter, "runtime scheduler error: {error}"),
            Self::Specialist(error) => write!(formatter, "runtime specialist error: {error}"),
            Self::Capability(error) => write!(formatter, "runtime capability error: {error}"),
            Self::Tool(error) => write!(formatter, "runtime tool error: {error}"),
            Self::FailureMemory(error) => {
                write!(formatter, "runtime failure-memory error: {error}")
            }
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::EventStore(error)
    }
}

impl From<ArtifactError> for RuntimeError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

impl From<CacheError> for RuntimeError {
    fn from(error: CacheError) -> Self {
        Self::Cache(error)
    }
}

impl From<ContextError> for RuntimeError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

impl From<IpcError> for RuntimeError {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

impl From<AssumptionError> for RuntimeError {
    fn from(error: AssumptionError) -> Self {
        Self::Assumption(error)
    }
}

impl From<SchedulerError> for RuntimeError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler(error)
    }
}

impl From<SpecialistError> for RuntimeError {
    fn from(error: SpecialistError) -> Self {
        Self::Specialist(error)
    }
}

impl From<CapabilityError> for RuntimeError {
    fn from(error: CapabilityError) -> Self {
        Self::Capability(error)
    }
}

impl From<ToolError> for RuntimeError {
    fn from(error: ToolError) -> Self {
        Self::Tool(error)
    }
}

impl From<FailureMemoryError> for RuntimeError {
    fn from(error: FailureMemoryError) -> Self {
        Self::FailureMemory(error)
    }
}

#[derive(Clone, Debug)]
pub struct RecoveredRun {
    pub run_id: RunId,
    pub state: RuntimeState,
    pub context: ContextGraph,
    pub events: Vec<StoredEvent>,
    pub cache_telemetry: CacheTelemetry,
    pub messages: Vec<IpcEnvelope>,
    pub assumptions: AssumptionGraph,
    pub scheduler: SchedulerState,
    pub capabilities: CapabilityPolicy,
    pub tools: ToolHistory,
    pub failures: FailureMemory,
    pub specialists: SpecialistRegistry,
    pub manager: ManagerProjection,
}

impl RecoveredRun {
    /// Rank model options from the telemetry recovered for this run.
    pub fn rank_cache_candidates(
        &self,
        prefix_hash: [u8; 32],
        candidates: &[ModelRouteCandidate],
        policy: CacheRoutingPolicy,
    ) -> Vec<RankedModelRoute> {
        rank_cache_candidates_from_telemetry(&self.cache_telemetry, prefix_hash, candidates, policy)
    }

    /// Rank model options using an explicit timestamp for cache-evidence
    /// freshness. This keeps expiry deterministic and replay-independent.
    pub fn rank_cache_candidates_at(
        &self,
        prefix_hash: [u8; 32],
        candidates: &[ModelRouteCandidate],
        policy: CacheRoutingPolicy,
        now_ms: u128,
    ) -> Vec<RankedModelRoute> {
        rank_cache_candidates_from_telemetry_at(
            &self.cache_telemetry,
            prefix_hash,
            candidates,
            policy,
            Some(now_ms),
        )
    }

    pub fn inspect_context(&self, policy: ContextFreshnessPolicy) -> ContextProprioception {
        self.context.proprioception(policy)
    }

    pub fn search_context(
        &self,
        principal: ContextPrincipal,
        request: &ContextSearchRequest,
    ) -> Result<Vec<ContextSearchHit>, ContextError> {
        self.context.search(principal, request)
    }

    pub fn context_dependencies(
        &self,
        reference: ContextRef,
        max_dependents: usize,
    ) -> Result<ContextDependencyReport, ContextError> {
        self.context.dependency_report(reference, max_dependents)
    }

    pub fn select_specialist(
        &self,
        request: &SpecialistSelectionRequest,
    ) -> Result<Option<SpecialistProfile>, SpecialistError> {
        self.specialists
            .select(request)
            .map(|profile| profile.cloned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagerAgentProjection {
    pub agent_id: AgentId,
    pub name: String,
    pub mission: String,
    pub model: ModelRef,
    pub status: AgentStatus,
    pub chunks_received: u32,
    pub usage: Usage,
    pub parent_id: Option<AgentId>,
    pub child_ids: Vec<AgentId>,
    pub health: orynth_scheduler::HealthState,
    pub budget: Option<BudgetState>,
    pub owned_resources: Vec<String>,
    pub assumption_ids: Vec<AssumptionId>,
    pub assumption_origins: Vec<(AssumptionId, TrustOrigin)>,
    pub conflict_ids: Vec<ConflictId>,
    pub failure_ids: Vec<FailureId>,
    pub active_failure_ids: Vec<FailureId>,
    pub specialist: Option<SpecialistProfile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagerProjection {
    pub run_id: RunId,
    pub agents: BTreeMap<AgentId, ManagerAgentProjection>,
    /// Bounded context-pressure and freshness state for the manager view.
    pub context: ContextProprioception,
    pub cache_observation_count: usize,
    pub artifact_count: usize,
    pub active_failure_count: usize,
}

struct ManagerProjectionSources<'a> {
    context: &'a ContextGraph,
    cache_telemetry: &'a CacheTelemetry,
    failures: &'a FailureMemory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupervisionDecision {
    NoAction,
    Promoted { model: ModelRef },
    Paused,
}

impl ManagerProjection {
    fn from_state(
        run_id: RunId,
        state: &RuntimeState,
        sources: ManagerProjectionSources<'_>,
        scheduler: &SchedulerState,
        assumptions: &AssumptionGraph,
        specialists: &SpecialistRegistry,
    ) -> Self {
        let ManagerProjectionSources {
            context,
            cache_telemetry,
            failures,
        } = sources;
        let agents = state
            .agents
            .iter()
            .map(|(agent_id, agent)| {
                let owned_resources = scheduler
                    .ownership()
                    .iter()
                    .filter(|(_, owner)| *owner == agent_id)
                    .map(|(resource, _)| resource.clone())
                    .collect();
                let assumption_ids = assumptions
                    .assumptions()
                    .iter()
                    .filter(|(_, assumption)| assumption.owner == *agent_id)
                    .map(|(assumption_id, _)| *assumption_id)
                    .collect();
                let assumption_origins = assumptions
                    .assumptions()
                    .iter()
                    .filter(|(_, assumption)| assumption.owner == *agent_id)
                    .map(|(assumption_id, assumption)| (*assumption_id, assumption.trust))
                    .collect();
                let conflict_ids = assumptions
                    .conflicts()
                    .iter()
                    .filter(|(_, conflict)| conflict.affected_agents.contains(agent_id))
                    .map(|(conflict_id, _)| *conflict_id)
                    .collect();
                let failure_ids = failures
                    .records()
                    .iter()
                    .filter(|(_, failure)| failure.agent_id == *agent_id)
                    .map(|(failure_id, _)| *failure_id)
                    .collect();
                let active_failure_ids = failures
                    .active_records()
                    .filter(|failure| failure.agent_id == *agent_id)
                    .map(|failure| failure.id)
                    .collect();
                (
                    *agent_id,
                    ManagerAgentProjection {
                        agent_id: *agent_id,
                        name: agent.identity.name.clone(),
                        mission: agent.identity.mission.clone(),
                        model: agent.model.clone(),
                        status: agent.status,
                        chunks_received: agent.chunks_received,
                        usage: agent.usage,
                        parent_id: scheduler.parent_of(*agent_id),
                        child_ids: scheduler.children_of(*agent_id),
                        health: scheduler.health(*agent_id),
                        budget: scheduler.budget(*agent_id),
                        owned_resources,
                        assumption_ids,
                        assumption_origins,
                        conflict_ids,
                        failure_ids,
                        active_failure_ids,
                        specialist: specialists.profile(*agent_id).cloned(),
                    },
                )
            })
            .collect();
        Self {
            run_id,
            agents,
            context: context.proprioception(ContextFreshnessPolicy::default()),
            cache_observation_count: cache_telemetry.len(),
            artifact_count: state.artifacts.len(),
            active_failure_count: failures.active_records().count(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExternalizedContext {
    pub log: ContextEventLog,
    pub artifact_events: Vec<Event>,
}

pub fn externalize_context_log<A: ArtifactStore>(
    run_id: RunId,
    log: &ContextEventLog,
    artifacts: &mut A,
    inline_limit: usize,
) -> Result<ExternalizedContext, RuntimeError> {
    let mut transitions = Vec::with_capacity(log.events().len());
    let mut created_artifacts = BTreeSet::new();
    let mut artifact_events = Vec::new();

    for transition in log.events() {
        match transition {
            ContextTransition::Created { block, content } if content.len() > inline_limit => {
                let artifact = artifacts.put_with_trust(
                    CONTEXT_ARTIFACT_MEDIA_TYPE,
                    content.clone(),
                    block.trust,
                )?;
                let artifact_hash = artifact.content_hash.as_bytes();
                if created_artifacts.insert(artifact_hash) {
                    artifact_events.push(artifact.created_event(run_id));
                }
                transitions.push(ContextTransition::CreatedFromArtifact {
                    block: block.clone(),
                    artifact_hash,
                });
            }
            other => transitions.push(other.clone()),
        }
    }

    Ok(ExternalizedContext {
        log: ContextEventLog::from_transitions(transitions),
        artifact_events,
    })
}

pub struct RuntimeService<S> {
    event_store: S,
    cache_telemetry: CacheTelemetry,
    mailbox_capacity: usize,
    mailboxes: BTreeMap<orynth_kernel::AgentId, BoundedMailbox>,
    assumptions: AssumptionGraph,
    scheduler: SchedulerState,
    capabilities: CapabilityPolicy,
    tools: ToolHistory,
    failures: FailureMemory,
    specialists: SpecialistRegistry,
}

impl<S> RuntimeService<S> {
    pub fn new(event_store: S) -> Self {
        Self {
            event_store,
            cache_telemetry: CacheTelemetry::new(),
            mailbox_capacity: DEFAULT_MAILBOX_CAPACITY,
            mailboxes: BTreeMap::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
        }
    }

    pub fn with_mailbox_capacity(event_store: S, capacity: usize) -> Result<Self, RuntimeError> {
        BoundedMailbox::new(capacity)?;
        Ok(Self {
            event_store,
            cache_telemetry: CacheTelemetry::new(),
            mailbox_capacity: capacity,
            mailboxes: BTreeMap::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
        })
    }

    pub fn event_store(&self) -> &S {
        &self.event_store
    }

    pub fn event_store_mut(&mut self) -> &mut S {
        &mut self.event_store
    }

    pub fn cache_telemetry(&self) -> &CacheTelemetry {
        &self.cache_telemetry
    }

    pub fn assumptions(&self) -> &AssumptionGraph {
        &self.assumptions
    }

    pub fn scheduler(&self) -> &SchedulerState {
        &self.scheduler
    }

    pub fn capabilities(&self) -> &CapabilityPolicy {
        &self.capabilities
    }

    pub fn tools(&self) -> &ToolHistory {
        &self.tools
    }

    pub fn failures(&self) -> &FailureMemory {
        &self.failures
    }

    pub fn specialists(&self) -> &SpecialistRegistry {
        &self.specialists
    }

    pub fn record_tool_transition(
        &mut self,
        run_id: RunId,
        transition: ToolTransition,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if let ToolTransition::Proposed { proposal, .. } = &transition {
            if proposal.run_id != run_id {
                return Err(StoreError::InvalidTransition(
                    "tool proposal belongs to another run".to_owned(),
                )
                .into());
            }
            if !state.agents.contains_key(&proposal.agent_id) {
                return Err(StoreError::UnknownAgent(proposal.agent_id).into());
            }
            if let Some(task_id) = proposal.task_id
                && !state.tasks.contains_key(&task_id)
            {
                return Err(StoreError::InvalidTransition(format!(
                    "tool proposal references unknown task {task_id}"
                ))
                .into());
            }
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = ToolHistory::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::ToolTransition {
                version: TOOL_SCHEMA_VERSION,
                payload: encode_tool_transition(&transition)?,
            },
        ))?;
        self.tools = candidate;
        Ok(())
    }

    /// Persist a compact failed-approach fact without copying a transcript.
    pub fn record_failure(
        &mut self,
        run_id: RunId,
        record: FailureRecord,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&record.agent_id) {
            return Err(StoreError::UnknownAgent(record.agent_id).into());
        }
        if let Some(task_id) = record.task_id
            && !state.tasks.contains_key(&task_id)
        {
            return Err(StoreError::InvalidTransition(format!(
                "failure record references unknown task {task_id}"
            ))
            .into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = FailureMemory::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = FailureTransition::Recorded { record };
        candidate.apply(&transition)?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::FailureMemoryTransition {
                version: FAILURE_MEMORY_SCHEMA_VERSION,
                payload: orynth_failure_memory::encode_transition(&transition)?,
            },
        ))?;
        self.failures = candidate;
        Ok(())
    }

    pub fn resolve_failure(
        &mut self,
        run_id: RunId,
        failure_id: orynth_kernel::FailureId,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        self.event_store.reconstruct(run_id)?;
        let events = self.event_store.events(run_id)?;
        let mut candidate = FailureMemory::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = FailureTransition::Resolved { failure_id };
        candidate.apply(&transition)?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::FailureMemoryTransition {
                version: FAILURE_MEMORY_SCHEMA_VERSION,
                payload: orynth_failure_memory::encode_transition(&transition)?,
            },
        ))?;
        self.failures = candidate;
        Ok(())
    }

    pub fn grant_capability(
        &mut self,
        run_id: RunId,
        lease: CapabilityLease,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&lease.agent_id) {
            return Err(StoreError::UnknownAgent(lease.agent_id).into());
        }
        if let Some(task_id) = lease.task_id
            && !state.tasks.contains_key(&task_id)
        {
            return Err(StoreError::InvalidTransition(format!(
                "capability lease references unknown task {task_id}"
            ))
            .into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = CapabilityPolicy::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = CapabilityTransition::Granted { lease };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::CapabilityTransition {
                version: CAPABILITY_SCHEMA_VERSION,
                payload: encode_capability_transition(&transition)?,
            },
        ))?;
        self.capabilities = candidate;
        Ok(())
    }

    pub fn revoke_capability(
        &mut self,
        run_id: RunId,
        agent_id: AgentId,
        task_id: Option<orynth_kernel::TaskId>,
        domain: orynth_security::CapabilityDomain,
        resource: impl Into<String>,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = CapabilityPolicy::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = CapabilityTransition::Revoked {
            agent_id,
            task_id,
            domain,
            resource: resource.into(),
        };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::CapabilityTransition {
                version: CAPABILITY_SCHEMA_VERSION,
                payload: encode_capability_transition(&transition)?,
            },
        ))?;
        self.capabilities = candidate;
        Ok(())
    }

    pub fn observe_cache_usage(
        &mut self,
        model: &ModelRef,
        prefix_hash: [u8; 32],
        estimated_prefix_tokens: u64,
        usage: Usage,
        observed_at_ms: u128,
    ) -> Result<Option<CacheObservation>, RuntimeError> {
        self.cache_telemetry
            .record(
                model,
                prefix_hash,
                estimated_prefix_tokens,
                usage,
                observed_at_ms,
            )
            .map_err(RuntimeError::from)
    }

    pub fn send_message(&mut self, envelope: IpcEnvelope) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        envelope.validate()?;
        let state = self.event_store.reconstruct(envelope.run_id)?;
        let runtime_sender = envelope.sender == RUNTIME_AGENT_ID
            && matches!(envelope.provenance, IpcProvenance::Runtime);
        if !state.agents.contains_key(&envelope.sender) && !runtime_sender {
            return Err(StoreError::UnknownAgent(envelope.sender).into());
        }
        if !state.agents.contains_key(&envelope.recipient) {
            return Err(StoreError::UnknownAgent(envelope.recipient).into());
        }
        let recipient = envelope.recipient;
        let mailbox = self
            .mailboxes
            .entry(recipient)
            .or_insert(BoundedMailbox::new(self.mailbox_capacity)?);
        if mailbox.is_full() {
            return Err(IpcError::MailboxFull { recipient }.into());
        }
        let payload = envelope.encode()?;
        self.event_store.append(Event::new(
            envelope.run_id,
            EventKind::AgentMessage {
                version: IPC_SCHEMA_VERSION,
                payload,
            },
        ))?;
        mailbox.try_send(envelope)?;
        Ok(())
    }

    /// Send a bounded peer-consultation question through the durable IPC
    /// boundary and return its message identity for correlation.
    pub fn request_consultation(
        &mut self,
        run_id: RunId,
        requester: AgentId,
        consultant: AgentId,
        subject: impl Into<String>,
        why: impl Into<String>,
    ) -> Result<MessageId, RuntimeError>
    where
        S: EventStore,
    {
        let envelope = IpcEnvelope::new(
            run_id,
            None,
            requester,
            consultant,
            IpcMessage::Question {
                subject: subject.into(),
                why: why.into(),
            },
        )
        .with_provenance(IpcProvenance::Agent);
        let message_id = envelope.id;
        self.send_message(envelope)?;
        Ok(message_id)
    }

    /// Send a bounded answer to a peer consultation. Correlation remains
    /// explicit in the subject/evidence or caller-provided causal events.
    #[allow(clippy::too_many_arguments)]
    pub fn answer_consultation(
        &mut self,
        run_id: RunId,
        answerer: AgentId,
        requester: AgentId,
        subject: impl Into<String>,
        value: impl Into<String>,
        revision: Option<String>,
        evidence: Vec<String>,
    ) -> Result<MessageId, RuntimeError>
    where
        S: EventStore,
    {
        let envelope = IpcEnvelope::new(
            run_id,
            None,
            answerer,
            requester,
            IpcMessage::Answer {
                subject: subject.into(),
                value: value.into(),
                revision,
                evidence,
            },
        )
        .with_provenance(IpcProvenance::Agent);
        let message_id = envelope.id;
        self.send_message(envelope)?;
        Ok(message_id)
    }

    pub fn receive_message(&mut self, recipient: orynth_kernel::AgentId) -> Option<IpcEnvelope> {
        self.mailboxes
            .get_mut(&recipient)
            .and_then(BoundedMailbox::receive)
    }

    pub fn pending_messages(&self, recipient: orynth_kernel::AgentId) -> usize {
        self.mailboxes
            .get(&recipient)
            .map_or(0, BoundedMailbox::len)
    }

    pub fn publish_assumption(
        &mut self,
        assumption: Assumption,
    ) -> Result<AssumptionPublication, RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(assumption.run_id)?;
        if !state.agents.contains_key(&assumption.owner) {
            return Err(StoreError::UnknownAgent(assumption.owner).into());
        }
        let existing_events = self.event_store.events(assumption.run_id)?;
        let existing_events = existing_events
            .iter()
            .map(|stored| stored.event.clone())
            .collect::<Vec<_>>();
        let mut candidate = AssumptionGraph::from_events(&existing_events)?;
        let publication = candidate.publish(assumption)?;
        let mut events = Vec::with_capacity(publication.transitions.len());
        for transition in &publication.transitions {
            events.push(Event::new(
                publication.assumption.run_id,
                EventKind::AssumptionTransition {
                    version: ASSUMPTION_SCHEMA_VERSION,
                    payload: encode_transition(transition)?,
                },
            ));
        }
        let causal_event_ids = events.iter().map(|event| event.id).collect::<Vec<_>>();
        let mut notifications = Vec::new();
        for conflict in &publication.conflicts {
            let left_assumption = candidate
                .assumptions()
                .get(&conflict.left)
                .ok_or(AssumptionError::Unknown(conflict.left))?;
            let right_assumption = candidate
                .assumptions()
                .get(&conflict.right)
                .ok_or(AssumptionError::Unknown(conflict.right))?;
            let left = left_assumption.normalized_value.clone();
            let right = right_assumption.normalized_value.clone();
            let affected = conflict
                .affected_agents
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            for recipient in &conflict.affected_agents {
                if !state.agents.contains_key(recipient) {
                    return Err(StoreError::UnknownAgent(*recipient).into());
                }
                let envelope = IpcEnvelope::new(
                    publication.assumption.run_id,
                    None,
                    RUNTIME_AGENT_ID,
                    *recipient,
                    IpcMessage::Conflict {
                        subject: conflict.subject.clone(),
                        left: left.clone(),
                        right: right.clone(),
                        affected: affected.clone(),
                    },
                )
                .with_causal_events(causal_event_ids.clone())
                .with_provenance(IpcProvenance::Runtime)
                .with_input_origin(left_assumption.trust)
                .with_input_origin(right_assumption.trust);
                let payload = envelope.encode()?;
                events.push(Event::new(
                    publication.assumption.run_id,
                    EventKind::AgentMessage {
                        version: IPC_SCHEMA_VERSION,
                        payload,
                    },
                ));
                notifications.push(envelope);
            }
        }
        let mut pending_by_recipient = BTreeMap::new();
        for envelope in &notifications {
            *pending_by_recipient.entry(envelope.recipient).or_insert(0) += 1;
        }
        for (recipient, pending) in pending_by_recipient {
            let queued = self.pending_messages(recipient);
            if queued.saturating_add(pending) > self.mailbox_capacity {
                return Err(IpcError::MailboxFull { recipient }.into());
            }
        }
        self.event_store.append_batch(&events)?;
        self.assumptions = candidate;
        for envelope in notifications {
            self.mailboxes
                .entry(envelope.recipient)
                .or_insert(BoundedMailbox::new(self.mailbox_capacity)?)
                .try_send(envelope)?;
        }
        Ok(publication)
    }

    pub fn configure_budget(
        &mut self,
        run_id: RunId,
        agent_id: orynth_kernel::AgentId,
        limits: BudgetLimits,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition =
            orynth_scheduler::SchedulerTransition::BudgetConfigured { agent_id, limits };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }

    pub fn transfer_budget(
        &mut self,
        run_id: RunId,
        from_agent: orynth_kernel::AgentId,
        to_agent: orynth_kernel::AgentId,
        limits: BudgetLimits,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&from_agent) {
            return Err(StoreError::UnknownAgent(from_agent).into());
        }
        if !state.agents.contains_key(&to_agent) {
            return Err(StoreError::UnknownAgent(to_agent).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = orynth_scheduler::SchedulerTransition::BudgetTransferred {
            from_agent,
            to_agent,
            limits,
        };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }

    pub fn record_agent_usage(
        &mut self,
        run_id: RunId,
        agent_id: orynth_kernel::AgentId,
        delta: BudgetUsage,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = orynth_scheduler::SchedulerTransition::UsageRecorded { agent_id, delta };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }

    pub fn record_health_signal(
        &mut self,
        run_id: RunId,
        agent_id: orynth_kernel::AgentId,
        signal: HealthSignal,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = orynth_scheduler::SchedulerTransition::HealthSignaled { agent_id, signal };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }

    pub fn select_model(
        &mut self,
        run_id: RunId,
        agent_id: AgentId,
        model: ModelRef,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        self.event_store.append(Event::new(
            run_id,
            EventKind::ModelRequested { agent_id, model },
        ))?;
        Ok(())
    }

    pub fn spawn_agent(
        &mut self,
        run_id: RunId,
        parent_id: AgentId,
        child: AgentIdentity,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&parent_id) {
            return Err(StoreError::UnknownAgent(parent_id).into());
        }
        if state.agents.contains_key(&child.id) {
            return Err(StoreError::InvalidTransition(format!(
                "agent {} already exists",
                child.id
            ))
            .into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let usage = orynth_scheduler::SchedulerTransition::UsageRecorded {
            agent_id: parent_id,
            delta: BudgetUsage {
                child_agents: 1,
                ..BudgetUsage::default()
            },
        };
        let relationship = orynth_scheduler::SchedulerTransition::ChildSpawned {
            parent_id,
            child_id: child.id,
        };
        candidate.apply(usage.clone())?;
        candidate.apply(relationship.clone())?;
        self.event_store.append_batch(&[
            Event::new(run_id, EventKind::AgentCreated { agent: child }),
            Event::new(
                run_id,
                EventKind::SchedulerTransition {
                    version: SCHEDULER_SCHEMA_VERSION,
                    payload: orynth_scheduler::encode_transition(&usage)?,
                },
            ),
            Event::new(
                run_id,
                EventKind::SchedulerTransition {
                    version: SCHEDULER_SCHEMA_VERSION,
                    payload: orynth_scheduler::encode_transition(&relationship)?,
                },
            ),
        ])?;
        self.scheduler = candidate;
        Ok(())
    }

    /// Create a child with a bounded, durable specialist profile.
    ///
    /// The identity, parent budget charge, parent/child relationship, and
    /// profile are committed in one event-store batch. A failed profile
    /// validation or budget check therefore leaves no partially-created
    /// specialist behind.
    pub fn spawn_specialist(
        &mut self,
        run_id: RunId,
        parent_id: AgentId,
        child: AgentIdentity,
        profile: SpecialistProfile,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        if profile.agent_id != child.id {
            return Err(SpecialistError::Invalid(
                "specialist profile agent does not match child identity".to_owned(),
            )
            .into());
        }
        profile.validate()?;
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&parent_id) {
            return Err(StoreError::UnknownAgent(parent_id).into());
        }
        if state.agents.contains_key(&child.id) {
            return Err(StoreError::InvalidTransition(format!(
                "agent {} already exists",
                child.id
            ))
            .into());
        }
        let events = self.event_store.events(run_id)?;
        let event_values = events
            .iter()
            .map(|stored| stored.event.clone())
            .collect::<Vec<_>>();
        let mut candidate_scheduler = SchedulerState::from_events(&event_values)?;
        let usage = orynth_scheduler::SchedulerTransition::UsageRecorded {
            agent_id: parent_id,
            delta: BudgetUsage {
                child_agents: 1,
                ..BudgetUsage::default()
            },
        };
        let relationship = orynth_scheduler::SchedulerTransition::ChildSpawned {
            parent_id,
            child_id: child.id,
        };
        candidate_scheduler.apply(usage.clone())?;
        candidate_scheduler.apply(relationship.clone())?;

        let mut candidate_specialists = SpecialistRegistry::from_events(&event_values)?;
        let specialist = SpecialistTransition::Registered { profile };
        candidate_specialists.apply(&specialist)?;

        self.event_store.append_batch(&[
            Event::new(run_id, EventKind::AgentCreated { agent: child }),
            Event::new(
                run_id,
                EventKind::SchedulerTransition {
                    version: SCHEDULER_SCHEMA_VERSION,
                    payload: orynth_scheduler::encode_transition(&usage)?,
                },
            ),
            Event::new(
                run_id,
                EventKind::SchedulerTransition {
                    version: SCHEDULER_SCHEMA_VERSION,
                    payload: orynth_scheduler::encode_transition(&relationship)?,
                },
            ),
            Event::new(
                run_id,
                EventKind::SpecialistTransition {
                    version: SPECIALIST_SCHEMA_VERSION,
                    payload: orynth_specialist::encode_transition(&specialist)?,
                },
            ),
        ])?;
        self.scheduler = candidate_scheduler;
        self.specialists = candidate_specialists;
        Ok(())
    }

    pub fn pause_agent(&mut self, run_id: RunId, agent_id: AgentId) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        self.event_store
            .append(Event::new(run_id, EventKind::AgentPaused { agent_id }))?;
        Ok(())
    }

    pub fn resume_agent(&mut self, run_id: RunId, agent_id: AgentId) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        self.event_store
            .append(Event::new(run_id, EventKind::AgentResumed { agent_id }))?;
        Ok(())
    }

    /// Cancel an agent through the durable model-cancellation transition.
    /// Logical identity and all recovered projections remain intact.
    pub fn cancel_agent(&mut self, run_id: RunId, agent_id: AgentId) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        let agent = state
            .agents
            .get(&agent_id)
            .ok_or(StoreError::UnknownAgent(agent_id))?;
        if matches!(
            agent.status,
            AgentStatus::Completed | AgentStatus::Cancelled | AgentStatus::Failed
        ) {
            return Err(StoreError::InvalidTransition(format!(
                "agent {agent_id} cannot be cancelled from {:?}",
                agent.status
            ))
            .into());
        }
        self.event_store
            .append(Event::new(run_id, EventKind::ModelCancelled { agent_id }))?;
        Ok(())
    }

    pub fn supervise_agent(
        &mut self,
        run_id: RunId,
        agent_id: AgentId,
    ) -> Result<SupervisionDecision, RuntimeError>
    where
        S: EventStore,
    {
        self.supervise_agent_with_policy(run_id, agent_id, SupervisionPolicy::default(), &[], false)
    }

    /// Evaluate deterministic supervision policy against recovered state.
    ///
    /// Candidates and the user-pin bit are explicit caller inputs. The
    /// supervisor never discovers providers, infers model quality, or
    /// overrides a pinned model. A promotion preserves the logical `AgentId`
    /// and is recorded as the same durable model-selection transition used by
    /// explicit runtime control.
    pub fn supervise_agent_with_policy(
        &mut self,
        run_id: RunId,
        agent_id: AgentId,
        policy: SupervisionPolicy,
        candidates: &[ModelRef],
        user_pinned: bool,
    ) -> Result<SupervisionDecision, RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        let agent = state
            .agents
            .get(&agent_id)
            .ok_or(StoreError::UnknownAgent(agent_id))?;
        if agent.status == AgentStatus::Paused
            || matches!(
                agent.status,
                AgentStatus::Completed | AgentStatus::Cancelled | AgentStatus::Failed
            )
        {
            return Ok(SupervisionDecision::NoAction);
        }
        let events = self.event_store.events(run_id)?;
        let event_values = events
            .iter()
            .map(|stored| stored.event.clone())
            .collect::<Vec<_>>();
        let scheduler = SchedulerState::from_events(&event_values)?;
        let specialists = SpecialistRegistry::from_events(&event_values)?;
        let promotable = specialists
            .profile(agent_id)
            .is_some_and(|profile| profile.promotable);
        match choose_supervision_action(
            scheduler.health(agent_id),
            &agent.model,
            promotable,
            user_pinned,
            candidates,
            policy,
        )? {
            SupervisionAction::NoAction => Ok(SupervisionDecision::NoAction),
            SupervisionAction::Promote(model) => {
                self.select_model(run_id, agent_id, model.clone())?;
                Ok(SupervisionDecision::Promoted { model })
            }
            SupervisionAction::Pause => {
                self.pause_agent(run_id, agent_id)?;
                Ok(SupervisionDecision::Paused)
            }
        }
    }

    pub fn claim_ownership(
        &mut self,
        run_id: RunId,
        agent_id: orynth_kernel::AgentId,
        resource: impl Into<String>,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = orynth_scheduler::SchedulerTransition::OwnershipClaimed {
            agent_id,
            resource: resource.into(),
        };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }

    pub fn release_ownership(
        &mut self,
        run_id: RunId,
        agent_id: orynth_kernel::AgentId,
        resource: impl Into<String>,
    ) -> Result<(), RuntimeError>
    where
        S: EventStore,
    {
        let state = self.event_store.reconstruct(run_id)?;
        if !state.agents.contains_key(&agent_id) {
            return Err(StoreError::UnknownAgent(agent_id).into());
        }
        let events = self.event_store.events(run_id)?;
        let mut candidate = SchedulerState::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        let transition = orynth_scheduler::SchedulerTransition::OwnershipReleased {
            agent_id,
            resource: resource.into(),
        };
        candidate.apply(transition.clone())?;
        self.event_store.append(Event::new(
            run_id,
            EventKind::SchedulerTransition {
                version: SCHEDULER_SCHEMA_VERSION,
                payload: orynth_scheduler::encode_transition(&transition)?,
            },
        ))?;
        self.scheduler = candidate;
        Ok(())
    }
}

impl<S: EventStore> RuntimeService<S> {
    pub fn record_cache_usage(
        &mut self,
        run_id: RunId,
        model: &ModelRef,
        prefix_hash: [u8; 32],
        estimated_prefix_tokens: u64,
        usage: Usage,
    ) -> Result<Option<CacheObservation>, RuntimeError> {
        let mut candidate = self.cache_telemetry.clone();
        let Some(observation) =
            candidate.record(model, prefix_hash, estimated_prefix_tokens, usage, now_ms())?
        else {
            return Ok(None);
        };
        self.event_store.append(Event::new(
            run_id,
            EventKind::CacheObserved {
                provider: model.provider.clone(),
                model: model.model.clone(),
                prefix_hash,
                estimated_prefix_tokens,
                cached_input_tokens: observation.observed_cached_tokens,
            },
        ))?;
        self.cache_telemetry = candidate;
        Ok(Some(observation))
    }

    /// Rank model options using only explicit provider cache observations for
    /// the exact rendered prefix. This is read-only and never infers a hit.
    pub fn rank_cache_candidates(
        &self,
        prefix_hash: [u8; 32],
        candidates: &[ModelRouteCandidate],
        policy: CacheRoutingPolicy,
    ) -> Vec<RankedModelRoute> {
        rank_cache_candidates_from_telemetry(&self.cache_telemetry, prefix_hash, candidates, policy)
    }

    /// Rank model options at an explicit timestamp so a freshness window can
    /// be applied without consulting an ambient clock.
    pub fn rank_cache_candidates_at(
        &self,
        prefix_hash: [u8; 32],
        candidates: &[ModelRouteCandidate],
        policy: CacheRoutingPolicy,
        now_ms: u128,
    ) -> Vec<RankedModelRoute> {
        rank_cache_candidates_from_telemetry_at(
            &self.cache_telemetry,
            prefix_hash,
            candidates,
            policy,
            Some(now_ms),
        )
    }

    /// Inspect context through the authoritative recovered graph without
    /// mutating lifecycle, access timestamps, or content.
    pub fn inspect_context(
        &self,
        run_id: RunId,
        policy: ContextFreshnessPolicy,
    ) -> Result<ContextProprioception, RuntimeError> {
        Ok(self.recover(run_id)?.inspect_context(policy))
    }

    pub fn search_context(
        &self,
        run_id: RunId,
        principal: ContextPrincipal,
        request: &ContextSearchRequest,
    ) -> Result<Vec<ContextSearchHit>, RuntimeError> {
        Ok(self.recover(run_id)?.search_context(principal, request)?)
    }

    pub fn context_dependencies(
        &self,
        run_id: RunId,
        reference: ContextRef,
        max_dependents: usize,
    ) -> Result<ContextDependencyReport, RuntimeError> {
        Ok(self
            .recover(run_id)?
            .context_dependencies(reference, max_dependents)?)
    }

    /// Select a recovered specialist by descriptive profile requirements.
    /// Selection is read-only; model routing and capability grants remain
    /// explicit policies.
    pub fn select_specialist(
        &self,
        run_id: RunId,
        request: &SpecialistSelectionRequest,
    ) -> Result<Option<SpecialistProfile>, RuntimeError>
    where
        S: EventStore,
    {
        let events = self.event_store.events(run_id)?;
        let registry = SpecialistRegistry::from_events(
            &events
                .iter()
                .map(|stored| stored.event.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(registry.select(request)?.cloned())
    }

    /// Archive a context block through an authoritative persisted transition.
    /// Use the artifact-store variant when the run contains externalized
    /// context content.
    pub fn archive_context(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover(run_id)?.context;
        let transition = context.archive(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    pub fn archive_context_with_artifact_store<A: ArtifactStore>(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
        artifacts: &A,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover_with_artifact_store(run_id, artifacts)?.context;
        let transition = context.archive(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    /// Restore an archived context block through an authoritative persisted
    /// transition. Restoration never invents content; it only changes the
    /// lifecycle of the existing immutable reference.
    pub fn restore_context(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover(run_id)?.context;
        let transition = context.restore(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    pub fn restore_context_with_artifact_store<A: ArtifactStore>(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
        artifacts: &A,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover_with_artifact_store(run_id, artifacts)?.context;
        let transition = context.restore(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    /// Pin a context block through an authoritative persisted transition.
    /// Pinning changes retention metadata only; it does not force projection.
    pub fn pin_context(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover(run_id)?.context;
        let transition = context.pin(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    pub fn pin_context_with_artifact_store<A: ArtifactStore>(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
        artifacts: &A,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover_with_artifact_store(run_id, artifacts)?.context;
        let transition = context.pin(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    /// Remove a context retention pin through an authoritative persisted
    /// transition without changing lifecycle or content.
    pub fn unpin_context(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover(run_id)?.context;
        let transition = context.unpin(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    pub fn unpin_context_with_artifact_store<A: ArtifactStore>(
        &mut self,
        run_id: RunId,
        reference: ContextRef,
        artifacts: &A,
    ) -> Result<(), RuntimeError> {
        let mut context = self.recover_with_artifact_store(run_id, artifacts)?.context;
        let transition = context.unpin(reference)?;
        self.append_context_lifecycle_transition(run_id, transition)
    }

    fn append_context_lifecycle_transition(
        &mut self,
        run_id: RunId,
        transition: ContextTransition,
    ) -> Result<(), RuntimeError> {
        self.event_store.append(Event::new(
            run_id,
            EventKind::ContextTransition {
                version: orynth_context::CONTEXT_TRANSITION_VERSION,
                payload: orynth_context::encode_transition(&transition)?,
            },
        ))?;
        Ok(())
    }

    pub fn recover(&self, run_id: RunId) -> Result<RecoveredRun, RuntimeError> {
        self.recover_with_resolver(run_id, |artifact_hash| {
            Err(ContextError::MissingArtifact(artifact_hash))
        })
    }

    pub fn recover_with_artifact_store<A: ArtifactStore>(
        &self,
        run_id: RunId,
        artifacts: &A,
    ) -> Result<RecoveredRun, RuntimeError> {
        self.recover_with_resolver(run_id, |artifact_hash| {
            let content_hash = ContentHash::from_digest(artifact_hash);
            match artifacts.get(content_hash) {
                Ok(Some(artifact)) => Ok(artifact.bytes().to_vec()),
                Ok(None) => Err(ContextError::MissingArtifact(artifact_hash)),
                Err(error) => Err(ContextError::ArtifactResolution(error.to_string())),
            }
        })
    }

    fn recover_with_resolver<F>(
        &self,
        run_id: RunId,
        resolver: F,
    ) -> Result<RecoveredRun, RuntimeError>
    where
        F: FnMut([u8; 32]) -> Result<Vec<u8>, ContextError>,
    {
        let events = self.event_store.events(run_id)?;
        let state = self.event_store.reconstruct(run_id)?;
        let context_events = events
            .iter()
            .map(|stored| stored.event.clone())
            .collect::<Vec<Event>>();
        let context =
            ContextEventLog::from_events(&context_events)?.replay_with_content(resolver)?;
        let assumptions = AssumptionGraph::from_events(&context_events)?;
        let scheduler = SchedulerState::from_events(&context_events)?;
        let capabilities = CapabilityPolicy::from_events(&context_events)?;
        let tools = ToolHistory::from_events(&context_events)?;
        let failures = FailureMemory::from_events(&context_events)?;
        let specialists = SpecialistRegistry::from_events(&context_events)?;
        let mut cache_telemetry = CacheTelemetry::new();
        let mut messages = Vec::new();
        for stored in &events {
            if let EventKind::CacheObserved {
                provider,
                model,
                prefix_hash,
                estimated_prefix_tokens,
                cached_input_tokens,
            } = &stored.event.kind
            {
                cache_telemetry.record_observation(CacheObservation {
                    key: CacheKey {
                        provider: provider.clone(),
                        model: model.clone(),
                        prefix_hash: *prefix_hash,
                    },
                    estimated_prefix_tokens: *estimated_prefix_tokens,
                    observed_cached_tokens: *cached_input_tokens,
                    observed_at_ms: stored.event.occurred_at_ms,
                });
            }
            if let EventKind::AgentMessage { version, payload } = &stored.event.kind {
                messages.push(IpcEnvelope::decode(*version, payload)?);
            }
        }
        let manager = ManagerProjection::from_state(
            run_id,
            &state,
            ManagerProjectionSources {
                context: &context,
                cache_telemetry: &cache_telemetry,
                failures: &failures,
            },
            &scheduler,
            &assumptions,
            &specialists,
        );
        Ok(RecoveredRun {
            run_id,
            state,
            context,
            events,
            cache_telemetry,
            messages,
            assumptions,
            scheduler,
            capabilities,
            tools,
            failures,
            specialists,
            manager,
        })
    }
}

fn rank_cache_candidates_from_telemetry(
    telemetry: &CacheTelemetry,
    prefix_hash: [u8; 32],
    candidates: &[ModelRouteCandidate],
    policy: CacheRoutingPolicy,
) -> Vec<RankedModelRoute> {
    rank_cache_candidates_from_telemetry_at(telemetry, prefix_hash, candidates, policy, None)
}

fn rank_cache_candidates_from_telemetry_at(
    telemetry: &CacheTelemetry,
    prefix_hash: [u8; 32],
    candidates: &[ModelRouteCandidate],
    policy: CacheRoutingPolicy,
    now_ms: Option<u128>,
) -> Vec<RankedModelRoute> {
    let decorated = candidates.iter().map(|candidate| {
        let record = telemetry.get(&CacheKey::new(&candidate.model, prefix_hash));
        CacheAwareRouteCandidate {
            model: candidate.model.clone(),
            estimated_cost_micros: candidate.estimated_cost_micros,
            observed_cached_tokens: record.map(|record| record.last_cached_tokens),
            observed_at_ms: record.map(|record| record.last_observed_at_ms),
        }
    });
    match now_ms {
        Some(now_ms) => rank_cache_aware_candidates_at(decorated, policy, now_ms),
        None => rank_cache_aware_candidates(decorated, policy),
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_assumptions::{Assumption, AssumptionState};
    use orynth_context::{
        ContextDraft, ContextKind, ContextOwner, ContextPrincipal, ContextScope,
        ContextSearchRequest, TrustLevel,
    };
    use orynth_event_store::{
        FileEventStore, InMemoryArtifactStore, InMemoryEventStore, SqliteArtifactStore,
        SqliteEventStore,
    };
    use orynth_failure_memory::{FailureRecord, FailureState};
    use orynth_ipc::{IpcEnvelope, IpcMessage, IpcProvenance};
    use orynth_kernel::{AgentIdentity, EventKind, EventTrace, ModelClass, ModelRef, TrustOrigin};
    use orynth_security::{CapabilityDomain, CapabilityError, CapabilityLease};
    use orynth_tool_runtime::{
        EffectClass, RepairAudit, RepairChange, RepairTier, ToolPreview, ToolProposal,
        ToolProvenance, ToolState, ToolTransition,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    fn temp_path(label: &str) -> PathBuf {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "orynth-runtime-{label}-{}-{id}.db",
            std::process::id()
        ))
    }

    fn context_log() -> ContextEventLog {
        context_log_with_trust(TrustLevel::Generated)
    }

    fn context_log_with_trust(trust: TrustLevel) -> ContextEventLog {
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(
                ContextDraft::new(
                    "project.rules",
                    ContextKind::Contract,
                    ContextOwner::Runtime,
                    ContextScope::Global,
                    b"typed runtime state".to_vec(),
                )
                .with_trust(trust),
            )
            .expect("context publication should succeed");
        let mut log = ContextEventLog::new();
        log.record(&publication);
        log
    }

    fn context_trace() -> (RunId, EventTrace) {
        let run_id = RunId::new();
        let log = context_log();

        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        for event in log.to_events(run_id).expect("context events should encode") {
            trace.record(event);
        }
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));
        (run_id, trace)
    }

    fn assert_recovered(service: &RuntimeService<InMemoryEventStore>, run_id: RunId) {
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.run_id, run_id);
        assert_eq!(
            recovered.state.status,
            orynth_event_store::RunStatus::Completed
        );
        let reference = recovered
            .context
            .latest("project.rules")
            .expect("context namespace should recover");
        assert_eq!(
            recovered.context.content(
                recovered
                    .context
                    .block(reference)
                    .expect("context block should recover")
                    .content_hash,
            ),
            Ok(b"typed runtime state".as_slice())
        );
    }

    #[test]
    fn runtime_service_hydrates_context_from_in_memory_events() {
        let (run_id, trace) = context_trace();
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(trace.events())
            .expect("trace should append");

        assert_recovered(&service, run_id);
    }

    #[test]
    fn runtime_service_hydrates_context_after_filesystem_reopen() {
        let path = temp_path("filesystem");
        let (run_id, trace) = context_trace();
        {
            let mut service = RuntimeService::new(
                FileEventStore::open(&path).expect("filesystem store should open"),
            );
            service
                .event_store_mut()
                .append_batch(trace.events())
                .expect("trace should append");
        }

        let service = RuntimeService::new(
            FileEventStore::open(&path).expect("filesystem store should reopen"),
        );
        let recovered = service.recover(run_id).expect("run should recover");
        assert!(recovered.context.latest("project.rules").is_some());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db.lock"));
    }

    #[test]
    fn runtime_service_hydrates_context_after_sqlite_reopen() {
        let path = temp_path("sqlite");
        let (run_id, trace) = context_trace();
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(trace.events())
                .expect("trace should append");
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        assert!(recovered.context.latest("project.rules").is_some());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn runtime_context_inspection_search_and_dependencies_are_read_only() {
        let run_id = RunId::new();
        let owner = AgentId::new();
        let mut graph = ContextGraph::new();
        let schema = graph
            .publish(ContextDraft::new(
                "schema.users",
                ContextKind::Contract,
                ContextOwner::Agent(owner),
                ContextScope::Team,
                b"id: UUID".to_vec(),
            ))
            .expect("schema should publish");
        let dependent = graph
            .publish(
                ContextDraft::new(
                    "auth.contract",
                    ContextKind::Contract,
                    ContextOwner::Agent(owner),
                    ContextScope::Private(owner),
                    b"JWT subject uses users.id".to_vec(),
                )
                .with_dependency(schema.block.reference()),
            )
            .expect("dependent should publish");
        let mut events = vec![Event::new(run_id, EventKind::RunCreated { run_id })];
        let mut log = ContextEventLog::new();
        log.record(&schema);
        log.record(&dependent);
        events.extend(log.to_events(run_id).expect("context events should encode"));
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&events)
            .expect("run and context should persist");

        let dashboard = service
            .inspect_context(run_id, ContextFreshnessPolicy::default())
            .expect("dashboard should recover");
        assert_eq!(dashboard.active_blocks, 2);
        let hits = service
            .search_context(
                run_id,
                ContextPrincipal::Agent(owner),
                &ContextSearchRequest::new("jwt"),
            )
            .expect("search should recover");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].summary.reference, dependent.block.reference());
        let report = service
            .context_dependencies(run_id, schema.block.reference(), 8)
            .expect("dependency report should recover");
        assert_eq!(report.dependents, vec![dependent.block.reference()]);
        assert_eq!(
            service.event_store().events(run_id).expect("events").len(),
            3
        );
    }

    #[test]
    fn context_archive_and_restore_are_durable_runtime_operations() {
        let path = temp_path("sqlite-context-lifecycle");
        let run_id = RunId::new();
        let mut graph = ContextGraph::new();
        let publication = graph
            .publish(ContextDraft::new(
                "project.rules",
                ContextKind::Contract,
                ContextOwner::Runtime,
                ContextScope::Global,
                b"recoverable rules".to_vec(),
            ))
            .expect("context should publish");
        let reference = publication.block.reference();
        let mut log = ContextEventLog::new();
        log.record(&publication);
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            let mut events = vec![Event::new(run_id, EventKind::RunCreated { run_id })];
            events.extend(log.to_events(run_id).expect("context events should encode"));
            service
                .event_store_mut()
                .append_batch(&events)
                .expect("run and context should persist");
            service
                .archive_context(run_id, reference)
                .expect("context should archive");
            assert_eq!(
                service
                    .recover(run_id)
                    .expect("run should recover")
                    .context
                    .block(reference)
                    .expect("archived block")
                    .lifecycle,
                orynth_context::ContextLifecycle::Archived
            );
        }
        let mut service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        assert_eq!(
            service
                .recover(run_id)
                .expect("archived run should recover")
                .context
                .block(reference)
                .expect("archived block")
                .lifecycle,
            orynth_context::ContextLifecycle::Archived
        );
        service
            .restore_context(run_id, reference)
            .expect("context should restore");
        assert_eq!(
            service
                .recover(run_id)
                .expect("restored run should recover")
                .context
                .block(reference)
                .expect("restored block")
                .lifecycle,
            orynth_context::ContextLifecycle::Active
        );
        service
            .pin_context(run_id, reference)
            .expect("context should pin");
        assert!(
            service
                .recover(run_id)
                .expect("pinned run should recover")
                .context
                .block(reference)
                .expect("pinned block")
                .pinned
        );
        service
            .unpin_context(run_id, reference)
            .expect("context should unpin");
        assert!(
            !service
                .recover(run_id)
                .expect("unpinned run should recover")
                .context
                .block(reference)
                .expect("unpinned block")
                .pinned
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn runtime_service_reports_unknown_runs_from_the_authoritative_store() {
        let service = RuntimeService::new(InMemoryEventStore::new());
        assert!(matches!(
            service.recover(RunId::new()),
            Err(RuntimeError::EventStore(StoreError::UnknownRun(_)))
        ));
    }

    #[test]
    fn runtime_service_rejects_malformed_context_events_during_recovery() {
        let run_id = RunId::new();
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        trace.record(Event::new(
            run_id,
            EventKind::ContextTransition {
                version: 99,
                payload: vec![0],
            },
        ));
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(trace.events())
            .expect("opaque event should be durably appendable");
        assert!(matches!(
            service.recover(run_id),
            Err(RuntimeError::Context(ContextError::InvalidEncoding(_)))
        ));
    }

    #[test]
    fn large_context_content_round_trips_through_the_artifact_store() {
        let run_id = RunId::new();
        let log = context_log_with_trust(TrustLevel::WebUntrusted);
        let mut artifacts = InMemoryArtifactStore::new();
        let externalized = externalize_context_log(run_id, &log, &mut artifacts, 1)
            .expect("large context content should externalize");
        assert_eq!(externalized.artifact_events.len(), 1);
        assert!(matches!(
            &externalized.artifact_events[0].kind,
            EventKind::ArtifactCreated {
                trust: TrustOrigin::WebUntrusted,
                ..
            }
        ));
        assert!(matches!(
            externalized.log.events().first(),
            Some(ContextTransition::CreatedFromArtifact { .. })
        ));

        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        for event in externalized.artifact_events {
            trace.record(event);
        }
        for event in externalized
            .log
            .to_events(run_id)
            .expect("externalized context should encode")
        {
            trace.record(event);
        }
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));

        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(trace.events())
            .expect("externalized trace should append");
        assert!(matches!(
            service.recover(run_id),
            Err(RuntimeError::Context(ContextError::MissingArtifact(_)))
        ));
        let recovered = service
            .recover_with_artifact_store(run_id, &artifacts)
            .expect("artifact-backed context should recover");
        let reference = recovered
            .context
            .latest("project.rules")
            .expect("context namespace should recover");
        assert_eq!(
            recovered
                .context
                .content(
                    recovered
                        .context
                        .block(reference)
                        .expect("context block should recover")
                        .content_hash,
                )
                .expect("context content should resolve"),
            b"typed runtime state"
        );
    }

    #[test]
    fn artifact_backed_context_survives_sqlite_reopen() {
        let event_path = temp_path("sqlite-context-events");
        let artifact_path = temp_path("sqlite-context-artifacts");
        let run_id = RunId::new();
        let log = context_log();
        let externalized = {
            let mut artifacts =
                SqliteArtifactStore::open(&artifact_path).expect("artifact store should open");
            externalize_context_log(run_id, &log, &mut artifacts, 1)
                .expect("context should externalize")
        };
        let mut trace = EventTrace::default();
        trace.record(Event::new(run_id, EventKind::RunCreated { run_id }));
        for event in &externalized.artifact_events {
            trace.record(event.clone());
        }
        for event in externalized
            .log
            .to_events(run_id)
            .expect("externalized context should encode")
        {
            trace.record(event);
        }
        trace.record(Event::new(run_id, EventKind::RunCompleted { run_id }));
        {
            let mut events = SqliteEventStore::open(&event_path).expect("event store should open");
            events
                .append_batch(trace.events())
                .expect("externalized trace should append");
        }

        let events = SqliteEventStore::open(&event_path).expect("event store should reopen");
        let artifacts =
            SqliteArtifactStore::open(&artifact_path).expect("artifact store should reopen");
        let recovered = RuntimeService::new(events)
            .recover_with_artifact_store(run_id, &artifacts)
            .expect("artifact-backed context should recover after reopen");
        assert!(recovered.context.latest("project.rules").is_some());

        let _ = std::fs::remove_file(event_path);
        let _ = std::fs::remove_file(artifact_path);
    }

    #[test]
    fn runtime_cache_surface_records_only_explicit_provider_metadata() {
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        let model = ModelRef::new("provider", "model", ModelClass::Cheap);
        assert_eq!(
            service
                .observe_cache_usage(&model, [8; 32], 20, Usage::new(20, 2), 10)
                .expect("missing metadata should be accepted"),
            None
        );
        assert!(service.cache_telemetry().is_empty());

        let observation = service
            .observe_cache_usage(
                &model,
                [8; 32],
                20,
                Usage::new(20, 2).with_cached_input_tokens(15),
                20,
            )
            .expect("explicit metadata should be accepted")
            .expect("observation should be returned");
        assert_eq!(observation.observed_cached_tokens, 15);
        assert_eq!(service.cache_telemetry().len(), 1);
    }

    #[test]
    fn runtime_cache_routing_uses_exact_provider_observations() {
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        let warm = ModelRef::new("provider-a", "warm", ModelClass::Strong);
        let cold = ModelRef::new("provider-b", "cold", ModelClass::Cheap);
        let prefix_hash = [17; 32];
        service
            .observe_cache_usage(
                &warm,
                prefix_hash,
                100,
                Usage::new(100, 4).with_cached_input_tokens(50),
                20,
            )
            .expect("explicit cache metadata should be accepted");

        let ranked = service.rank_cache_candidates(
            prefix_hash,
            &[
                ModelRouteCandidate {
                    model: warm.clone(),
                    estimated_cost_micros: 100,
                },
                ModelRouteCandidate {
                    model: cold.clone(),
                    estimated_cost_micros: 20,
                },
            ],
            CacheRoutingPolicy {
                prefer_warm_cache: true,
                cached_token_value_micros: 2,
                max_observation_age_ms: None,
            },
        );
        assert_eq!(ranked[0].model, warm);
        assert_eq!(ranked[0].observed_cached_tokens, Some(50));
        assert_eq!(ranked[1].observed_cached_tokens, None);

        let unknown_prefix = service.rank_cache_candidates(
            [18; 32],
            &[
                ModelRouteCandidate {
                    model: ranked[0].model.clone(),
                    estimated_cost_micros: 100,
                },
                ModelRouteCandidate {
                    model: cold,
                    estimated_cost_micros: 20,
                },
            ],
            CacheRoutingPolicy::default(),
        );
        assert!(
            unknown_prefix
                .iter()
                .all(|route| route.observed_cached_tokens.is_none())
        );
        assert_eq!(unknown_prefix[0].estimated_cost_micros, 20);
    }

    #[test]
    fn runtime_cache_routing_applies_caller_supplied_expiry() {
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        let model = ModelRef::new("provider", "model", ModelClass::Cheap);
        let prefix_hash = [19; 32];
        service
            .observe_cache_usage(
                &model,
                prefix_hash,
                100,
                Usage::new(100, 1).with_cached_input_tokens(50),
                1_000,
            )
            .expect("explicit cache metadata should be accepted");
        let candidates = [ModelRouteCandidate {
            model,
            estimated_cost_micros: 100,
        }];
        let policy = CacheRoutingPolicy {
            prefer_warm_cache: true,
            cached_token_value_micros: 2,
            max_observation_age_ms: Some(100),
        };

        let fresh = service.rank_cache_candidates_at(prefix_hash, &candidates, policy, 1_100);
        assert!(fresh[0].cache_observation_fresh);
        assert_eq!(fresh[0].effective_cost_micros, 0);

        let stale = service.rank_cache_candidates_at(prefix_hash, &candidates, policy, 1_101);
        assert!(!stale[0].cache_observation_fresh);
        assert_eq!(stale[0].observed_cached_tokens, Some(50));
        assert_eq!(stale[0].effective_cost_micros, 100);
    }

    #[test]
    fn runtime_cache_observations_are_event_sourced_and_recovered() {
        let run_id = RunId::new();
        let model = ModelRef::new("provider", "model", ModelClass::Cheap);
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append(Event::new(run_id, EventKind::RunCreated { run_id }))
            .expect("run should be created");

        let observation = service
            .record_cache_usage(
                run_id,
                &model,
                [9; 32],
                20,
                Usage::new(20, 2).with_cached_input_tokens(15),
            )
            .expect("cache observation should append")
            .expect("explicit metadata should produce an observation");
        assert_eq!(observation.observed_cached_tokens, 15);
        assert_eq!(
            service.event_store().events(run_id).expect("events").len(),
            2
        );

        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.cache_telemetry.len(), 1);
        assert_eq!(
            recovered
                .cache_telemetry
                .records()
                .values()
                .next()
                .unwrap()
                .observations,
            1
        );
        let ranked = recovered.rank_cache_candidates(
            [9; 32],
            &[ModelRouteCandidate {
                model: model.clone(),
                estimated_cost_micros: 100,
            }],
            CacheRoutingPolicy::default(),
        );
        assert_eq!(ranked[0].observed_cached_tokens, Some(15));
    }

    #[test]
    fn runtime_cache_observations_survive_sqlite_reopen() {
        let path = temp_path("sqlite-cache");
        let run_id = RunId::new();
        let model = ModelRef::new("provider", "model", ModelClass::Cheap);
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append(Event::new(run_id, EventKind::RunCreated { run_id }))
                .expect("run should be created");
            service
                .record_cache_usage(
                    run_id,
                    &model,
                    [10; 32],
                    24,
                    Usage::new(24, 3).with_cached_input_tokens(18),
                )
                .expect("cache observation should append")
                .expect("explicit metadata should produce an observation");
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.cache_telemetry.len(), 1);
        assert_eq!(
            recovered
                .cache_telemetry
                .records()
                .values()
                .next()
                .expect("cache record")
                .last_cached_tokens,
            18
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tool_transactions_survive_sqlite_reopen() {
        let path = temp_path("sqlite-tools");
        let run_id = RunId::new();
        let task_id = orynth_kernel::TaskId::new();
        let agent = AgentIdentity::new(
            "tool-agent",
            "audit tools",
            ModelRef::new("provider", "model", ModelClass::Cheap),
        );
        let transaction_id = orynth_kernel::ToolTransactionId::from_u64(501);
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::TaskCreated {
                            task_id,
                            run_id,
                            title: "write audit".to_owned(),
                        },
                    ),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: agent.clone(),
                        },
                    ),
                ])
                .expect("run entities should append");
            service
                .record_tool_transition(
                    run_id,
                    ToolTransition::Proposed {
                        transaction_id,
                        proposal: ToolProposal {
                            run_id,
                            task_id: Some(task_id),
                            agent_id: agent.id,
                            tool_name: "fs.write".to_owned(),
                            input: [("path".to_owned(), "workspace/src/lib.rs".to_owned())]
                                .into_iter()
                                .collect(),
                            provenance: ToolProvenance::WebUntrusted("docs.example".to_owned()),
                            input_origins: Vec::new(),
                        },
                        state: ToolState::AwaitingApproval,
                    },
                )
                .expect("proposal should append");
            service
                .record_tool_transition(
                    run_id,
                    ToolTransition::Repaired {
                        transaction_id,
                        audit: RepairAudit {
                            tier: RepairTier::SyntaxSafe,
                            changes: vec![RepairChange::InputValueTrimmed {
                                field: "path".to_owned(),
                            }],
                        },
                    },
                )
                .expect("repair audit should append");
            service
                .record_tool_transition(
                    run_id,
                    ToolTransition::previewed(
                        transaction_id,
                        ToolPreview {
                            summary: "write one file".to_owned(),
                            resources: vec!["workspace/src/lib.rs".to_owned()],
                            operation_count: 1,
                            effect: EffectClass::Reversible,
                        },
                    )
                    .expect("preview should validate"),
                )
                .expect("preview audit should append");
            service
                .record_tool_transition(
                    run_id,
                    ToolTransition::state_changed(
                        transaction_id,
                        ToolState::Approved,
                        Some("approved by manager".to_owned()),
                    )
                    .expect("state transition should validate"),
                )
                .expect("state transition should append");
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        let record = recovered
            .tools
            .record(transaction_id)
            .expect("tool transaction should recover");
        assert_eq!(record.state, ToolState::Approved);
        assert_eq!(record.proposal.task_id, Some(task_id));
        assert_eq!(
            record.proposal.provenance,
            ToolProvenance::WebUntrusted("docs.example".to_owned())
        );
        assert_eq!(record.detail.as_deref(), Some("approved by manager"));
        assert!(record.repair.is_some());
        assert_eq!(record.preview.as_ref().unwrap().operation_count, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn budgets_and_health_are_durable_projections() {
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "perform bounded work",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let recipient = AgentIdentity::new(
            "recipient",
            "receive transferred capacity",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: recipient.clone(),
                    },
                ),
            ])
            .expect("run and agent should be created");
        service
            .configure_budget(
                run_id,
                agent.id,
                BudgetLimits {
                    max_tokens: Some(9),
                    ..BudgetLimits::default()
                },
            )
            .expect("budget should be configured");
        service
            .configure_budget(
                run_id,
                recipient.id,
                BudgetLimits {
                    max_tokens: Some(1),
                    ..BudgetLimits::default()
                },
            )
            .expect("recipient budget should be configured");
        service
            .transfer_budget(
                run_id,
                agent.id,
                recipient.id,
                BudgetLimits {
                    max_tokens: Some(4),
                    ..BudgetLimits::default()
                },
            )
            .expect("budget transfer should append");
        service
            .record_agent_usage(
                run_id,
                agent.id,
                BudgetUsage {
                    tokens: 5,
                    ..BudgetUsage::default()
                },
            )
            .expect("usage should fit budget");
        assert!(matches!(
            service.record_agent_usage(
                run_id,
                agent.id,
                BudgetUsage {
                    tokens: 1,
                    ..BudgetUsage::default()
                }
            ),
            Err(RuntimeError::Scheduler(
                SchedulerError::BudgetExceeded { .. }
            ))
        ));
        for _ in 0..3 {
            service
                .record_health_signal(run_id, agent.id, HealthSignal::Failure)
                .expect("health signal should append");
        }
        service
            .select_model(
                run_id,
                agent.id,
                ModelRef::new("mock", "strong", ModelClass::Strong),
            )
            .expect("model selection should append");

        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(
            recovered.scheduler.budget(agent.id).unwrap().usage.tokens,
            5
        );
        assert_eq!(
            recovered
                .scheduler
                .budget(agent.id)
                .unwrap()
                .limits
                .max_tokens,
            Some(5)
        );
        assert_eq!(
            recovered
                .scheduler
                .budget(recipient.id)
                .unwrap()
                .limits
                .max_tokens,
            Some(5)
        );
        assert_eq!(
            recovered.scheduler.health(agent.id).status,
            orynth_scheduler::HealthStatus::Blocked
        );
        let manager_agent = &recovered.manager.agents[&agent.id];
        assert_eq!(manager_agent.budget.unwrap().usage.tokens, 5);
        assert_eq!(
            manager_agent.health.status,
            orynth_scheduler::HealthStatus::Blocked
        );
        assert_eq!(manager_agent.model.model, "strong");
        assert_eq!(recovered.manager.context.active_blocks, 0);
        assert_eq!(recovered.manager.cache_observation_count, 0);
        assert_eq!(recovered.manager.artifact_count, 0);
        assert_eq!(recovered.manager.active_failure_count, 0);
    }

    #[test]
    fn failure_memory_records_attempts_and_recovers_through_sqlite() {
        let path = temp_path("sqlite-failure-memory");
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "record failed approaches",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let task = orynth_kernel::Task::new(run_id, "repair authentication");
        let record = FailureRecord::new(
            agent.id,
            "auth-token-parser-v1",
            "replace token parsing with a permissive split",
            "the authentication regression test rejects malformed claims",
        )
        .with_task_id(task.id)
        .with_evidence(vec![
            "tests/authentication.rs:44 failed".to_owned(),
            "claim validation rejected an empty subject".to_owned(),
        ]);
        let failure_id = record.id;
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::TaskCreated {
                            task_id: task.id,
                            run_id,
                            title: task.title.clone(),
                        },
                    ),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: agent.clone(),
                        },
                    ),
                ])
                .expect("run, task, and agent should be created");
            service
                .record_failure(run_id, record)
                .expect("failure memory record should append");
        }
        let mut service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("failure should recover");
        let recovered_record = recovered
            .failures
            .records()
            .get(&failure_id)
            .expect("failure record should be present");
        assert_eq!(recovered_record.state, FailureState::Active);
        assert_eq!(recovered_record.task_id, Some(task.id));
        assert!(recovered.failures.has_attempted("auth-token-parser-v1"));
        assert_eq!(recovered.manager.active_failure_count, 1);
        assert_eq!(
            recovered.manager.agents[&agent.id].failure_ids,
            vec![failure_id]
        );
        assert_eq!(
            recovered.manager.agents[&agent.id].active_failure_ids,
            vec![failure_id]
        );
        assert_eq!(
            recovered
                .failures
                .matching_fingerprint("auth-token-parser-v1")
                .len(),
            1
        );
        service
            .resolve_failure(run_id, failure_id)
            .expect("failure resolution should append");
        let recovered = service.recover(run_id).expect("resolution should recover");
        assert_eq!(
            recovered.failures.records()[&failure_id].state,
            FailureState::Resolved
        );
        assert_eq!(recovered.failures.active_records().count(), 0);
        assert_eq!(recovered.manager.active_failure_count, 0);
        assert!(
            recovered.manager.agents[&agent.id]
                .active_failure_ids
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn capability_leases_are_runtime_scoped_and_recoverable() {
        let path = temp_path("sqlite-capabilities");
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "use a scoped filesystem capability",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let task = orynth_kernel::Task::new(run_id, "write a source file");
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::TaskCreated {
                            task_id: task.id,
                            run_id,
                            title: task.title.clone(),
                        },
                    ),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: agent.clone(),
                        },
                    ),
                ])
                .expect("run, task, and agent should be created");
            service
                .grant_capability(
                    run_id,
                    CapabilityLease {
                        agent_id: agent.id,
                        task_id: Some(task.id),
                        domain: CapabilityDomain::Filesystem,
                        resource: "workspace/src".to_owned(),
                        expires_at_ms: 100,
                    },
                )
                .expect("capability should be granted");
        }
        let mut service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        assert!(
            recovered
                .capabilities
                .authorize(
                    agent.id,
                    Some(task.id),
                    CapabilityDomain::Filesystem,
                    "workspace/src/lib.rs",
                    50,
                )
                .is_ok()
        );
        service
            .revoke_capability(
                run_id,
                agent.id,
                Some(task.id),
                CapabilityDomain::Filesystem,
                "workspace/src",
            )
            .expect("capability should be revoked");
        let recovered = service.recover(run_id).expect("run should recover");
        assert!(matches!(
            recovered.capabilities.authorize(
                agent.id,
                Some(task.id),
                CapabilityDomain::Filesystem,
                "workspace/src/lib.rs",
                50,
            ),
            Err(CapabilityError::Missing { .. })
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn budgets_and_health_survive_sqlite_reopen() {
        let path = temp_path("sqlite-scheduler");
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "persist coordination state",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: agent.clone(),
                        },
                    ),
                ])
                .expect("run and agent should be created");
            service
                .configure_budget(
                    run_id,
                    agent.id,
                    BudgetLimits {
                        max_tool_calls: Some(2),
                        ..BudgetLimits::default()
                    },
                )
                .expect("budget should be configured");
            service
                .record_agent_usage(
                    run_id,
                    agent.id,
                    BudgetUsage {
                        tool_calls: 1,
                        ..BudgetUsage::default()
                    },
                )
                .expect("usage should append");
            service
                .record_health_signal(run_id, agent.id, HealthSignal::ContextPressure)
                .expect("health signal should append");
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(
            recovered
                .scheduler
                .budget(agent.id)
                .unwrap()
                .usage
                .tool_calls,
            1
        );
        assert_eq!(
            recovered.scheduler.health(agent.id).status,
            orynth_scheduler::HealthStatus::Degraded
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ownership_is_membership_checked_and_recoverable() {
        let run_id = RunId::new();
        let first = AgentIdentity::new(
            "api",
            "own API resources",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let second = AgentIdentity::new(
            "ui",
            "own UI resources",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let unknown = orynth_kernel::AgentId::new();
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: first.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: second.clone(),
                    },
                ),
            ])
            .expect("run and agents should be created");
        service
            .claim_ownership(run_id, first.id, "repo:api")
            .expect("first owner should claim resource");
        assert!(matches!(
            service.claim_ownership(run_id, second.id, "repo:api"),
            Err(RuntimeError::Scheduler(
                SchedulerError::OwnershipConflict { .. }
            ))
        ));
        assert!(matches!(
            service.claim_ownership(run_id, unknown, "repo:other"),
            Err(RuntimeError::EventStore(StoreError::UnknownAgent(id))) if id == unknown
        ));
        let owned = service.recover(run_id).expect("run should recover");
        assert_eq!(
            owned.manager.agents[&first.id].owned_resources,
            vec!["repo:api".to_owned()]
        );
        service
            .release_ownership(run_id, first.id, "repo:api")
            .expect("owner should release resource");
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.scheduler.owner("repo:api"), None);
    }

    #[test]
    fn blocked_health_triggers_replayable_supervision_pause() {
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "pause after repeated failures",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
            ])
            .expect("run and agent should be created");
        for _ in 0..3 {
            service
                .record_health_signal(run_id, agent.id, HealthSignal::Failure)
                .expect("health signal should append");
        }
        assert_eq!(
            service
                .supervise_agent(run_id, agent.id)
                .expect("supervision should evaluate"),
            SupervisionDecision::Paused
        );
        assert_eq!(
            service
                .supervise_agent(run_id, agent.id)
                .expect("paused agent should not be paused twice"),
            SupervisionDecision::NoAction
        );
        let paused = service.recover(run_id).expect("run should recover");
        assert_eq!(paused.state.agents[&agent.id].status, AgentStatus::Paused);
        assert_eq!(paused.manager.agents[&agent.id].status, AgentStatus::Paused);
        service
            .resume_agent(run_id, agent.id)
            .expect("paused agent should resume");
        let resumed = service.recover(run_id).expect("run should recover");
        assert_eq!(resumed.state.agents[&agent.id].status, AgentStatus::Running);
    }

    #[test]
    fn cancellation_is_durable_and_preserves_logical_agent_identity() {
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "cancelable worker",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: agent.clone(),
                    },
                ),
            ])
            .expect("run and agent should be created");
        service
            .cancel_agent(run_id, agent.id)
            .expect("agent should cancel");
        let cancelled = service.recover(run_id).expect("run should recover");
        assert_eq!(
            cancelled.state.agents[&agent.id].status,
            AgentStatus::Cancelled
        );
        assert_eq!(cancelled.state.agents[&agent.id].identity, agent);
        assert!(matches!(
            service.cancel_agent(run_id, agent.id),
            Err(RuntimeError::EventStore(StoreError::InvalidTransition(_)))
        ));
    }

    #[test]
    fn paused_status_survives_sqlite_reopen() {
        let path = temp_path("sqlite-supervision");
        let run_id = RunId::new();
        let agent = AgentIdentity::new(
            "worker",
            "persist supervision state",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: agent.clone(),
                        },
                    ),
                ])
                .expect("run and agent should be created");
            service
                .pause_agent(run_id, agent.id)
                .expect("agent should pause");
        }
        let mut service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let paused = service.recover(run_id).expect("paused run should recover");
        assert_eq!(paused.state.agents[&agent.id].status, AgentStatus::Paused);
        service
            .resume_agent(run_id, agent.id)
            .expect("agent should resume");
        let resumed = service.recover(run_id).expect("resumed run should recover");
        assert_eq!(resumed.state.agents[&agent.id].status, AgentStatus::Running);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn child_spawn_is_budgeted_and_recoverable() {
        let run_id = RunId::new();
        let parent = AgentIdentity::new(
            "manager",
            "coordinate specialists",
            ModelRef::new("mock", "strong", ModelClass::Strong),
        );
        let child = AgentIdentity::new(
            "specialist",
            "implement a bounded slice",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let second_child = AgentIdentity::new(
            "reviewer",
            "review the bounded slice",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: parent.clone(),
                    },
                ),
            ])
            .expect("run and parent should be created");
        service
            .configure_budget(
                run_id,
                parent.id,
                BudgetLimits {
                    max_child_agents: Some(1),
                    ..BudgetLimits::default()
                },
            )
            .expect("parent budget should be configured");
        service
            .spawn_agent(run_id, parent.id, child.clone())
            .expect("first child should spawn");
        let events_before = service
            .event_store()
            .events(run_id)
            .expect("events should be readable")
            .len();
        assert!(matches!(
            service.spawn_agent(run_id, parent.id, second_child),
            Err(RuntimeError::Scheduler(
                SchedulerError::BudgetExceeded { .. }
            ))
        ));
        assert_eq!(
            service
                .event_store()
                .events(run_id)
                .expect("events should be readable")
                .len(),
            events_before
        );
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.state.agents.len(), 2);
        assert_eq!(
            recovered.manager.agents[&child.id].parent_id,
            Some(parent.id)
        );
        assert_eq!(
            recovered.manager.agents[&parent.id].child_ids,
            vec![child.id]
        );
        assert_eq!(
            recovered
                .scheduler
                .budget(parent.id)
                .unwrap()
                .usage
                .child_agents,
            1
        );
    }

    #[test]
    fn specialist_spawn_is_atomic_and_survives_sqlite_reopen() {
        let path = temp_path("sqlite-specialist");
        let run_id = RunId::new();
        let parent = AgentIdentity::new(
            "manager",
            "coordinate specialists",
            ModelRef::new("mock", "strong", ModelClass::Strong),
        );
        let child = AgentIdentity::new(
            "AUTH-01",
            "own authentication and session implementation",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let profile = SpecialistProfile::new(child.id, "Authentication Specialist")
            .with_scope(vec!["src/auth/**".to_owned()])
            .with_subscriptions(vec!["schema.users.*".to_owned(), "api.auth.*".to_owned()])
            .with_capabilities(vec!["filesystem.read".to_owned(), "cargo test".to_owned()])
            .promotable(true);
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: parent.clone(),
                        },
                    ),
                ])
                .expect("run and parent should be created");
            service
                .configure_budget(
                    run_id,
                    parent.id,
                    BudgetLimits {
                        max_child_agents: Some(1),
                        ..BudgetLimits::default()
                    },
                )
                .expect("parent budget should be configured");
            service
                .spawn_specialist(run_id, parent.id, child.clone(), profile.clone())
                .expect("specialist should be atomically spawned");
            let events_before = service
                .event_store()
                .events(run_id)
                .expect("events should be readable")
                .len();
            let second_child = AgentIdentity::new(
                "DB-02",
                "own the database contract",
                ModelRef::new("mock", "cheap", ModelClass::Cheap),
            );
            let second_profile = SpecialistProfile::new(second_child.id, "Database Specialist");
            assert!(matches!(
                service.spawn_specialist(run_id, parent.id, second_child, second_profile),
                Err(RuntimeError::Scheduler(
                    SchedulerError::BudgetExceeded { .. }
                ))
            ));
            assert_eq!(
                service
                    .event_store()
                    .events(run_id)
                    .expect("events should be readable")
                    .len(),
                events_before
            );
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service
            .recover(run_id)
            .expect("specialist run should recover");
        assert_eq!(recovered.specialists.profile(child.id), Some(&profile));
        assert_eq!(
            recovered.manager.agents[&child.id].specialist,
            Some(profile.clone())
        );
        assert_eq!(
            recovered.manager.agents[&child.id].parent_id,
            Some(parent.id)
        );
        let selected = service
            .select_specialist(
                run_id,
                &SpecialistSelectionRequest::new("authentication specialist")
                    .with_scope(vec!["src/auth/**".to_owned()])
                    .with_capabilities(vec!["filesystem.read".to_owned()])
                    .promotable_only(true),
            )
            .expect("specialist selection should recover")
            .expect("matching specialist should exist");
        assert_eq!(selected, profile);
        assert!(
            recovered
                .events
                .iter()
                .any(|event| matches!(event.event.kind, EventKind::SpecialistTransition { .. }))
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn policy_supervision_promotes_a_promotable_specialist_without_changing_identity() {
        let run_id = RunId::new();
        let parent = AgentIdentity::new(
            "manager",
            "supervise workers",
            ModelRef::new("mock", "manager", ModelClass::Strong),
        );
        let child = AgentIdentity::new(
            "AUTH-01",
            "repair authentication",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let strong = ModelRef::new("mock", "strong", ModelClass::Strong);
        let profile =
            SpecialistProfile::new(child.id, "Authentication Specialist").promotable(true);
        let mut service = RuntimeService::new(InMemoryEventStore::new());
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: parent.clone(),
                    },
                ),
            ])
            .expect("run and manager should be created");
        service
            .spawn_specialist(run_id, parent.id, child.clone(), profile)
            .expect("specialist should spawn");
        for _ in 0..2 {
            service
                .record_health_signal(run_id, child.id, HealthSignal::Failure)
                .expect("failure signal should append");
        }
        assert_eq!(
            service
                .supervise_agent_with_policy(
                    run_id,
                    child.id,
                    SupervisionPolicy {
                        promote_after_failures: Some(2),
                        pause_when_blocked: true,
                    },
                    std::slice::from_ref(&strong),
                    false,
                )
                .expect("policy supervision should evaluate"),
            SupervisionDecision::Promoted {
                model: strong.clone()
            }
        );
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.state.agents[&child.id].model, strong);
        assert_eq!(recovered.state.agents[&child.id].identity.id, child.id);
        assert_eq!(
            recovered.manager.agents[&child.id].parent_id,
            Some(parent.id)
        );
        assert_eq!(
            recovered.manager.agents[&child.id]
                .health
                .consecutive_failures,
            2
        );

        let event_count = recovered.events.len();
        assert_eq!(
            service
                .supervise_agent_with_policy(
                    run_id,
                    child.id,
                    SupervisionPolicy {
                        promote_after_failures: Some(2),
                        pause_when_blocked: true,
                    },
                    &[ModelRef::new("mock", "stronger", ModelClass::Strong)],
                    true,
                )
                .expect("pinned policy supervision should evaluate"),
            SupervisionDecision::NoAction
        );
        assert_eq!(
            service.event_store().events(run_id).unwrap().len(),
            event_count
        );
    }

    #[test]
    fn typed_ipc_is_bounded_durable_and_recoverable() {
        let run_id = RunId::new();
        let sender = AgentIdentity::new(
            "sender",
            "ask a narrow contract question",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let recipient = AgentIdentity::new(
            "recipient",
            "answer contract questions",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::with_mailbox_capacity(InMemoryEventStore::new(), 1)
            .expect("mailbox capacity should be valid");
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: sender.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: recipient.clone(),
                    },
                ),
            ])
            .expect("run and agents should be created");

        let first = IpcEnvelope::new(
            run_id,
            None,
            sender.id,
            recipient.id,
            IpcMessage::Question {
                subject: "schema.users.id".to_owned(),
                why: "JWT subject depends on this contract".to_owned(),
            },
        )
        .with_provenance(IpcProvenance::Agent);
        service
            .send_message(first.clone())
            .expect("first message should persist and enqueue");
        assert_eq!(service.pending_messages(recipient.id), 1);
        assert_eq!(service.receive_message(recipient.id), Some(first.clone()));

        let second = IpcEnvelope::new(
            run_id,
            None,
            sender.id,
            recipient.id,
            IpcMessage::Progress {
                summary: "waiting for the answer".to_owned(),
                completed_millis: 250,
            },
        );
        service
            .send_message(second.clone())
            .expect("mailbox should be reusable after receive");
        let third = IpcEnvelope::new(
            run_id,
            None,
            sender.id,
            recipient.id,
            IpcMessage::Warning {
                subject: "schema.users.id".to_owned(),
                message: "contract is still unresolved".to_owned(),
            },
        );
        assert!(matches!(
            service.send_message(third),
            Err(RuntimeError::Ipc(orynth_ipc::IpcError::MailboxFull { recipient: id })) if id == recipient.id
        ));
        assert_eq!(service.pending_messages(recipient.id), 1);

        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.messages, vec![first, second]);
        assert_eq!(
            service.event_store().events(run_id).expect("events").len(),
            5
        );
    }

    #[test]
    fn peer_consultation_helpers_persist_narrow_question_and_answer_messages() {
        let run_id = RunId::new();
        let requester = AgentIdentity::new(
            "auth",
            "ask a contract question",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let consultant = AgentIdentity::new(
            "database",
            "answer contract questions",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::with_mailbox_capacity(InMemoryEventStore::new(), 2)
            .expect("mailbox capacity should be valid");
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: requester.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: consultant.clone(),
                    },
                ),
            ])
            .expect("run and agents should be created");

        let question_id = service
            .request_consultation(
                run_id,
                requester.id,
                consultant.id,
                "schema.users.id",
                "auth token subject depends on the database contract",
            )
            .expect("question should persist");
        let question = service
            .receive_message(consultant.id)
            .expect("consultant should receive question");
        assert_eq!(question.id, question_id);
        assert!(matches!(question.payload, IpcMessage::Question { .. }));

        let answer_id = service
            .answer_consultation(
                run_id,
                consultant.id,
                requester.id,
                "schema.users.id",
                "UUID",
                Some("schema.users@2".to_owned()),
                vec!["context://schema/users@2".to_owned()],
            )
            .expect("answer should persist");
        let answer = service
            .receive_message(requester.id)
            .expect("requester should receive answer");
        assert_eq!(answer.id, answer_id);
        assert!(matches!(answer.payload, IpcMessage::Answer { .. }));
        assert_eq!(
            service
                .recover(run_id)
                .expect("run should recover")
                .messages
                .len(),
            2
        );
    }

    #[test]
    fn typed_ipc_survives_sqlite_reopen() {
        let path = temp_path("sqlite-ipc");
        let run_id = RunId::new();
        let sender = AgentIdentity::new(
            "sender",
            "send a handoff",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let recipient = AgentIdentity::new(
            "recipient",
            "receive a handoff",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let message = IpcEnvelope::new(
            run_id,
            None,
            sender.id,
            recipient.id,
            IpcMessage::Handoff {
                summary: "database contract is ready for review".to_owned(),
            },
        );
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: sender.clone(),
                        },
                    ),
                    Event::new(run_id, EventKind::AgentCreated { agent: recipient }),
                ])
                .expect("run and agents should be created");
            service
                .send_message(message.clone())
                .expect("message should persist");
        }

        let service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.messages, vec![message]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn assumption_conflicts_notify_affected_agents() {
        let run_id = RunId::new();
        let first_owner = AgentIdentity::new(
            "auth",
            "own authentication contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let second_owner = AgentIdentity::new(
            "database",
            "own database contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::with_mailbox_capacity(InMemoryEventStore::new(), 2)
            .expect("mailbox capacity should be valid");
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: first_owner.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: second_owner.clone(),
                    },
                ),
            ])
            .expect("run and agents should be created");
        service
            .publish_assumption(Assumption::new(
                run_id,
                first_owner.id,
                "schema.users.id",
                "UUID",
                "users.id is UUID",
            ))
            .expect("first assumption should publish");
        let publication = service
            .publish_assumption(Assumption::new(
                run_id,
                second_owner.id,
                "schema.users.id",
                "BIGINT",
                "users.id is BIGINT",
            ))
            .expect("conflicting assumption should publish");
        assert_eq!(publication.conflicts.len(), 1);
        assert_eq!(service.pending_messages(first_owner.id), 1);
        assert_eq!(service.pending_messages(second_owner.id), 1);
        for recipient in [first_owner.id, second_owner.id] {
            let message = service
                .receive_message(recipient)
                .expect("affected owner should receive a notification");
            assert_eq!(message.sender, RUNTIME_AGENT_ID);
            assert_eq!(message.recipient, recipient);
            assert_eq!(message.provenance, IpcProvenance::Runtime);
            assert!(matches!(message.payload, IpcMessage::Conflict { .. }));
        }
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.messages.len(), 2);
    }

    #[test]
    fn assumption_notifications_apply_backpressure_atomically() {
        let run_id = RunId::new();
        let first_owner = AgentIdentity::new(
            "auth",
            "own authentication contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let second_owner = AgentIdentity::new(
            "database",
            "own database contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let mut service = RuntimeService::with_mailbox_capacity(InMemoryEventStore::new(), 1)
            .expect("mailbox capacity should be valid");
        service
            .event_store_mut()
            .append_batch(&[
                Event::new(run_id, EventKind::RunCreated { run_id }),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: first_owner.clone(),
                    },
                ),
                Event::new(
                    run_id,
                    EventKind::AgentCreated {
                        agent: second_owner.clone(),
                    },
                ),
            ])
            .expect("run and agents should be created");
        service
            .publish_assumption(Assumption::new(
                run_id,
                first_owner.id,
                "schema.users.id",
                "UUID",
                "users.id is UUID",
            ))
            .expect("first assumption should publish");
        service
            .send_message(IpcEnvelope::new(
                run_id,
                None,
                second_owner.id,
                first_owner.id,
                IpcMessage::Progress {
                    summary: "existing work is still running".to_owned(),
                    completed_millis: 100,
                },
            ))
            .expect("mailbox should accept the existing message");
        let events_before = service
            .event_store()
            .events(run_id)
            .expect("events should be readable")
            .len();
        let result = service.publish_assumption(Assumption::new(
            run_id,
            second_owner.id,
            "schema.users.id",
            "BIGINT",
            "users.id is BIGINT",
        ));
        assert!(matches!(
            result,
            Err(RuntimeError::Ipc(orynth_ipc::IpcError::MailboxFull { recipient }))
                if recipient == first_owner.id
        ));
        assert_eq!(
            service
                .event_store()
                .events(run_id)
                .expect("events should be readable")
                .len(),
            events_before
        );
        assert_eq!(service.assumptions().assumptions().len(), 1);
        assert_eq!(service.pending_messages(first_owner.id), 1);
        assert_eq!(service.pending_messages(second_owner.id), 0);
    }

    #[test]
    fn assumptions_are_conflict_checked_and_recovered_from_sqlite() {
        let path = temp_path("sqlite-assumptions");
        let run_id = RunId::new();
        let first_owner = AgentIdentity::new(
            "auth",
            "own authentication contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let second_owner = AgentIdentity::new(
            "database",
            "own database contracts",
            ModelRef::new("mock", "cheap", ModelClass::Cheap),
        );
        let first_owner_id = first_owner.id;
        let second_owner_id = second_owner.id;
        let first = Assumption::new(
            run_id,
            first_owner_id,
            "schema.users.id",
            "UUID",
            "users.id is UUID",
        );
        let second = Assumption::new(
            run_id,
            second_owner_id,
            " schema.users.id ",
            " BIGINT ",
            "users.id is BIGINT",
        )
        .with_trust(TrustOrigin::TrustedProject)
        .with_input_origin(TrustOrigin::WebUntrusted);
        let first_id = first.id;
        let second_id = second.id;
        {
            let mut service = RuntimeService::new(
                SqliteEventStore::open(&path).expect("SQLite store should open"),
            );
            service
                .event_store_mut()
                .append_batch(&[
                    Event::new(run_id, EventKind::RunCreated { run_id }),
                    Event::new(run_id, EventKind::AgentCreated { agent: first_owner }),
                    Event::new(
                        run_id,
                        EventKind::AgentCreated {
                            agent: second_owner,
                        },
                    ),
                ])
                .expect("run and agents should be created");
            service
                .publish_assumption(first)
                .expect("first assumption should publish");
            let publication = service
                .publish_assumption(second)
                .expect("conflicting assumption should publish");
            assert_eq!(publication.conflicts.len(), 1);
            assert_eq!(publication.transitions.len(), 3);
            assert_eq!(
                service.assumptions().assumptions()[&first_id].state,
                AssumptionState::Conflicted
            );
        }

        let mut service =
            RuntimeService::new(SqliteEventStore::open(&path).expect("SQLite store should reopen"));
        let third = Assumption::new(
            run_id,
            first_owner_id,
            "schema.users.id",
            "INTEGER",
            "users.id is INTEGER",
        );
        let publication = service
            .publish_assumption(third)
            .expect("reopened service should detect durable conflicts");
        assert_eq!(publication.conflicts.len(), 2);
        let recovered = service.recover(run_id).expect("run should recover");
        assert_eq!(recovered.assumptions.assumptions().len(), 3);
        assert_eq!(recovered.assumptions.conflicts().len(), 3);
        assert_eq!(recovered.messages.len(), 5);
        assert!(
            recovered
                .messages
                .iter()
                .all(|message| message.sender == RUNTIME_AGENT_ID
                    && message.provenance == IpcProvenance::Runtime
                    && matches!(message.payload, IpcMessage::Conflict { .. }))
        );
        assert!(
            recovered
                .messages
                .iter()
                .any(|message| message.effective_trust_origin() == TrustOrigin::WebUntrusted)
        );
        assert!(
            recovered.manager.agents[&second_owner_id]
                .assumption_origins
                .contains(&(second_id, TrustOrigin::WebUntrusted))
        );
        assert_eq!(
            recovered.assumptions.assumptions()[&first_id].state,
            AssumptionState::Conflicted
        );
        let _ = std::fs::remove_file(path);
    }
}
