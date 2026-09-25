//! Deterministic budget and health projections for coordinated agent runs.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use orynth_kernel::{AgentId, Event, EventKind, Usage};
use orynth_security::{
    OwnershipAccess, OwnershipError, ResourceOwnershipPolicy, canonical_resource, resources_overlap,
};

pub const SCHEDULER_SCHEMA_VERSION: u16 = 1;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_RESOURCE_BYTES: usize = 64 * 1024;

/// A model option presented to the deterministic routing policy.
///
/// `estimated_cost_micros` is caller-supplied policy data. It is not inferred
/// from model names or provider metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRouteCandidate {
    pub model: orynth_kernel::ModelRef,
    pub estimated_cost_micros: u64,
}

/// A candidate decorated with actual cache evidence, if the runtime has it.
/// `None` means no provider observation exists for this exact model/prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheAwareRouteCandidate {
    pub model: orynth_kernel::ModelRef,
    pub estimated_cost_micros: u64,
    pub observed_cached_tokens: Option<u64>,
    /// Provider observation time. It is required when the routing policy
    /// applies a caller-supplied freshness window.
    pub observed_at_ms: Option<u128>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CacheRoutingPolicy {
    pub prefer_warm_cache: bool,
    /// Estimated cost avoided per explicitly observed cached input token.
    pub cached_token_value_micros: u64,
    /// Maximum age accepted for cache evidence. `None` preserves evidence
    /// without applying an expiry rule; `Some` requires a routing timestamp.
    pub max_observation_age_ms: Option<u128>,
}

impl Default for CacheRoutingPolicy {
    fn default() -> Self {
        Self {
            prefer_warm_cache: true,
            cached_token_value_micros: 1,
            max_observation_age_ms: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RankedModelRoute {
    pub model: orynth_kernel::ModelRef,
    pub estimated_cost_micros: u64,
    pub observed_cached_tokens: Option<u64>,
    pub cache_observation_fresh: bool,
    pub effective_cost_micros: u64,
}

/// Rank model candidates without claiming a cache hit.
///
/// Only `Some` observations supplied by the runtime can contribute cache
/// value. An explicit zero remains an observation but contributes no savings.
/// Ties are resolved by provider, model, and model class for replayable
/// routing decisions.
pub fn rank_cache_aware_candidates(
    candidates: impl IntoIterator<Item = CacheAwareRouteCandidate>,
    policy: CacheRoutingPolicy,
) -> Vec<RankedModelRoute> {
    rank_cache_aware_candidates_at_inner(candidates, policy, None)
}

/// Rank candidates at an explicit caller-supplied time.
///
/// A freshness window is intentionally not inferred from provider names or
/// wall-clock access. When the policy has a maximum age, an observation is
/// valuable only when its timestamp is present, not in the future, and within
/// that window. Stale observations remain visible as evidence but cannot lower
/// the effective cost.
pub fn rank_cache_aware_candidates_at(
    candidates: impl IntoIterator<Item = CacheAwareRouteCandidate>,
    policy: CacheRoutingPolicy,
    now_ms: u128,
) -> Vec<RankedModelRoute> {
    rank_cache_aware_candidates_at_inner(candidates, policy, Some(now_ms))
}

fn rank_cache_aware_candidates_at_inner(
    candidates: impl IntoIterator<Item = CacheAwareRouteCandidate>,
    policy: CacheRoutingPolicy,
    now_ms: Option<u128>,
) -> Vec<RankedModelRoute> {
    let mut ranked = candidates
        .into_iter()
        .map(|candidate| {
            let cache_observation_fresh = cache_observation_is_fresh(
                candidate.observed_cached_tokens,
                candidate.observed_at_ms,
                policy.max_observation_age_ms,
                now_ms,
            );
            let cached_value = if policy.prefer_warm_cache && cache_observation_fresh {
                candidate
                    .observed_cached_tokens
                    .unwrap_or_default()
                    .saturating_mul(policy.cached_token_value_micros)
            } else {
                0
            };
            RankedModelRoute {
                model: candidate.model,
                estimated_cost_micros: candidate.estimated_cost_micros,
                observed_cached_tokens: candidate.observed_cached_tokens,
                cache_observation_fresh,
                effective_cost_micros: candidate.estimated_cost_micros.saturating_sub(cached_value),
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        let cache_tie_break = |route: &RankedModelRoute| {
            if policy.prefer_warm_cache && route.cache_observation_fresh {
                route.observed_cached_tokens.unwrap_or_default()
            } else {
                0
            }
        };
        left.effective_cost_micros
            .cmp(&right.effective_cost_micros)
            .then_with(|| cache_tie_break(right).cmp(&cache_tie_break(left)))
            .then_with(|| left.model.provider.cmp(&right.model.provider))
            .then_with(|| left.model.model.cmp(&right.model.model))
            .then_with(|| model_class_key(&left.model).cmp(&model_class_key(&right.model)))
    });
    ranked
}

fn cache_observation_is_fresh(
    observed_cached_tokens: Option<u64>,
    observed_at_ms: Option<u128>,
    max_age_ms: Option<u128>,
    now_ms: Option<u128>,
) -> bool {
    let Some(_) = observed_cached_tokens else {
        return false;
    };
    match max_age_ms {
        None => true,
        Some(max_age_ms) => match (observed_at_ms, now_ms) {
            (Some(observed_at_ms), Some(now_ms)) if observed_at_ms <= now_ms => {
                now_ms.saturating_sub(observed_at_ms) <= max_age_ms
            }
            _ => false,
        },
    }
}

fn model_class_key(model: &orynth_kernel::ModelRef) -> String {
    model.class.to_string()
}

/// Deterministic reactions available to a runtime supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisionPolicy {
    /// Promote after this many consecutive failure signals when a stronger
    /// candidate is available. `None` disables automatic promotion.
    pub promote_after_failures: Option<u32>,
    /// Pause an agent when health is blocked and no promotion was selected.
    pub pause_when_blocked: bool,
}

impl Default for SupervisionPolicy {
    fn default() -> Self {
        Self {
            promote_after_failures: None,
            pause_when_blocked: true,
        }
    }
}

impl SupervisionPolicy {
    pub fn validate(self) -> Result<(), SchedulerError> {
        if self.promote_after_failures == Some(0) {
            return Err(SchedulerError::Invalid(
                "promotion threshold must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupervisionAction {
    NoAction,
    Promote(orynth_kernel::ModelRef),
    Pause,
}

/// Choose one deterministic supervision action from recovered health and
/// caller-supplied model candidates. This function has no provider or clock
/// access and cannot change runtime state.
pub fn choose_supervision_action(
    health: HealthState,
    current_model: &orynth_kernel::ModelRef,
    promotable: bool,
    user_pinned: bool,
    candidates: &[orynth_kernel::ModelRef],
    policy: SupervisionPolicy,
) -> Result<SupervisionAction, SchedulerError> {
    policy.validate()?;
    if policy
        .promote_after_failures
        .is_some_and(|threshold| health.consecutive_failures >= threshold)
        && promotable
        && !user_pinned
    {
        let current_strength = model_class_strength(&current_model.class);
        let mut stronger = candidates
            .iter()
            .filter(|candidate| model_class_strength(&candidate.class) > current_strength)
            .cloned()
            .collect::<Vec<_>>();
        stronger.sort_by(|left, right| {
            model_class_strength(&left.class)
                .cmp(&model_class_strength(&right.class))
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.model.cmp(&right.model))
                .then_with(|| left.class.to_string().cmp(&right.class.to_string()))
        });
        if let Some(model) = stronger.into_iter().next() {
            return Ok(SupervisionAction::Promote(model));
        }
    }
    if policy.pause_when_blocked && health.status == HealthStatus::Blocked {
        return Ok(SupervisionAction::Pause);
    }
    Ok(SupervisionAction::NoAction)
}

fn model_class_strength(class: &orynth_kernel::ModelClass) -> u8 {
    match class {
        orynth_kernel::ModelClass::Local => 0,
        orynth_kernel::ModelClass::Cheap => 1,
        orynth_kernel::ModelClass::Custom(_) => 1,
        orynth_kernel::ModelClass::Strong => 2,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BudgetLimits {
    pub max_tokens: Option<u64>,
    pub max_money_micros: Option<u64>,
    pub max_wall_clock_ms: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_child_agents: Option<u64>,
    pub max_context_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BudgetUsage {
    pub tokens: u64,
    pub money_micros: u64,
    pub wall_clock_ms: u64,
    pub tool_calls: u64,
    pub child_agents: u64,
    pub context_tokens: u64,
}

impl BudgetUsage {
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            tokens: self.tokens.saturating_add(other.tokens),
            money_micros: self.money_micros.saturating_add(other.money_micros),
            wall_clock_ms: self.wall_clock_ms.saturating_add(other.wall_clock_ms),
            tool_calls: self.tool_calls.saturating_add(other.tool_calls),
            child_agents: self.child_agents.saturating_add(other.child_agents),
            context_tokens: self.context_tokens.saturating_add(other.context_tokens),
        }
    }

    pub fn from_model_usage(usage: Usage) -> Self {
        Self {
            tokens: usage.total_tokens(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetState {
    pub limits: BudgetLimits,
    pub usage: BudgetUsage,
}

impl BudgetState {
    fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            usage: BudgetUsage::default(),
        }
    }

    fn check(&self, usage: BudgetUsage, agent_id: AgentId) -> Result<(), SchedulerError> {
        let next = self.usage.saturating_add(usage);
        let checks = [
            (self.limits.max_tokens, next.tokens, BudgetDimension::Tokens),
            (
                self.limits.max_money_micros,
                next.money_micros,
                BudgetDimension::MoneyMicros,
            ),
            (
                self.limits.max_wall_clock_ms,
                next.wall_clock_ms,
                BudgetDimension::WallClockMs,
            ),
            (
                self.limits.max_tool_calls,
                next.tool_calls,
                BudgetDimension::ToolCalls,
            ),
            (
                self.limits.max_child_agents,
                next.child_agents,
                BudgetDimension::ChildAgents,
            ),
            (
                self.limits.max_context_tokens,
                next.context_tokens,
                BudgetDimension::ContextTokens,
            ),
        ];
        for (limit, observed, dimension) in checks {
            if let Some(limit) = limit
                && observed > limit
            {
                return Err(SchedulerError::BudgetExceeded {
                    agent_id,
                    dimension,
                    limit,
                    observed,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetDimension {
    Tokens,
    MoneyMicros,
    WallClockMs,
    ToolCalls,
    ChildAgents,
    ContextTokens,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HealthSignal {
    Failure,
    ToolError,
    NoProgress,
    ContextPressure,
    BudgetPressure,
    AssumptionConflict,
    VerificationFailure,
    DependencyInvalidation,
    Progress,
    FailureResolved,
    ToolErrorResolved,
    NoProgressResolved,
    ContextPressureResolved,
    BudgetPressureResolved,
    AssumptionConflictResolved,
    VerificationFailureResolved,
    DependencyInvalidationResolved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HealthState {
    pub status: HealthStatus,
    pub consecutive_failures: u32,
    pub consecutive_tool_errors: u32,
    pub no_progress: u32,
    pub context_pressure: u32,
    pub budget_pressure: u32,
    pub assumption_conflicts: u32,
    pub verification_failures: u32,
    pub dependency_invalidations: u32,
    /// Current unresolved pressure. The fields above remain historical
    /// counters; these fields drive current health.
    pub active_failures: u32,
    pub active_tool_errors: u32,
    pub active_no_progress: u32,
    pub active_context_pressure: u32,
    pub active_budget_pressure: u32,
    pub active_assumption_conflicts: u32,
    pub active_verification_failures: u32,
    pub active_dependency_invalidations: u32,
}

impl Default for HealthState {
    fn default() -> Self {
        Self {
            status: HealthStatus::Healthy,
            consecutive_failures: 0,
            consecutive_tool_errors: 0,
            no_progress: 0,
            context_pressure: 0,
            budget_pressure: 0,
            assumption_conflicts: 0,
            verification_failures: 0,
            dependency_invalidations: 0,
            active_failures: 0,
            active_tool_errors: 0,
            active_no_progress: 0,
            active_context_pressure: 0,
            active_budget_pressure: 0,
            active_assumption_conflicts: 0,
            active_verification_failures: 0,
            active_dependency_invalidations: 0,
        }
    }
}

impl HealthState {
    fn apply_signal(mut self, signal: HealthSignal) -> Self {
        match signal {
            HealthSignal::Failure => {
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                self.active_failures = self.active_failures.saturating_add(1);
            }
            HealthSignal::ToolError => {
                self.consecutive_tool_errors = self.consecutive_tool_errors.saturating_add(1);
                self.active_tool_errors = self.active_tool_errors.saturating_add(1);
            }
            HealthSignal::NoProgress => {
                self.no_progress = self.no_progress.saturating_add(1);
                self.active_no_progress = self.active_no_progress.saturating_add(1);
            }
            HealthSignal::ContextPressure => {
                self.context_pressure = self.context_pressure.saturating_add(1);
                self.active_context_pressure = self.active_context_pressure.saturating_add(1);
            }
            HealthSignal::BudgetPressure => {
                self.budget_pressure = self.budget_pressure.saturating_add(1);
                self.active_budget_pressure = self.active_budget_pressure.saturating_add(1);
            }
            HealthSignal::AssumptionConflict => {
                self.assumption_conflicts = self.assumption_conflicts.saturating_add(1);
                self.active_assumption_conflicts =
                    self.active_assumption_conflicts.saturating_add(1);
            }
            HealthSignal::VerificationFailure => {
                self.verification_failures = self.verification_failures.saturating_add(1);
                self.active_verification_failures =
                    self.active_verification_failures.saturating_add(1);
            }
            HealthSignal::DependencyInvalidation => {
                self.dependency_invalidations = self.dependency_invalidations.saturating_add(1);
                self.active_dependency_invalidations =
                    self.active_dependency_invalidations.saturating_add(1);
            }
            HealthSignal::Progress => {
                self.consecutive_failures = 0;
                self.consecutive_tool_errors = 0;
                self.no_progress = 0;
                self.active_failures = 0;
                self.active_tool_errors = 0;
                self.active_no_progress = 0;
            }
            HealthSignal::FailureResolved => {
                self.active_failures = self.active_failures.saturating_sub(1)
            }
            HealthSignal::ToolErrorResolved => {
                self.active_tool_errors = self.active_tool_errors.saturating_sub(1)
            }
            HealthSignal::NoProgressResolved => {
                self.active_no_progress = self.active_no_progress.saturating_sub(1)
            }
            HealthSignal::ContextPressureResolved => {
                self.active_context_pressure = self.active_context_pressure.saturating_sub(1)
            }
            HealthSignal::BudgetPressureResolved => {
                self.active_budget_pressure = self.active_budget_pressure.saturating_sub(1)
            }
            HealthSignal::AssumptionConflictResolved => {
                self.active_assumption_conflicts =
                    self.active_assumption_conflicts.saturating_sub(1)
            }
            HealthSignal::VerificationFailureResolved => {
                self.active_verification_failures =
                    self.active_verification_failures.saturating_sub(1)
            }
            HealthSignal::DependencyInvalidationResolved => {
                self.active_dependency_invalidations =
                    self.active_dependency_invalidations.saturating_sub(1)
            }
        }
        self.status = if self.active_dependency_invalidations > 0
            || self.active_failures >= 3
            || self.active_tool_errors >= 3
            || self.active_assumption_conflicts >= 2
            || self.active_verification_failures >= 3
        {
            HealthStatus::Blocked
        } else if self.active_failures > 0
            || self.active_tool_errors > 0
            || self.active_no_progress > 0
            || self.active_context_pressure > 0
            || self.active_budget_pressure > 0
            || self.active_assumption_conflicts > 0
            || self.active_verification_failures > 0
        {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        };
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerTransition {
    BudgetConfigured {
        agent_id: AgentId,
        limits: BudgetLimits,
    },
    UsageRecorded {
        agent_id: AgentId,
        delta: BudgetUsage,
    },
    HealthSignaled {
        agent_id: AgentId,
        signal: HealthSignal,
    },
    OwnershipClaimed {
        agent_id: AgentId,
        resource: String,
    },
    OwnershipReleased {
        agent_id: AgentId,
        resource: String,
    },
    ChildSpawned {
        parent_id: AgentId,
        child_id: AgentId,
    },
    BudgetTransferred {
        from_agent: AgentId,
        to_agent: AgentId,
        limits: BudgetLimits,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    Invalid(&'static str),
    UnsupportedVersion(u16),
    TooLarge(usize),
    BudgetExceeded {
        agent_id: AgentId,
        dimension: BudgetDimension,
        limit: u64,
        observed: u64,
    },
    OwnershipConflict {
        resource: String,
        owner: AgentId,
        requester: AgentId,
    },
    OwnershipNotHeld {
        resource: String,
        owner: AgentId,
    },
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid scheduler transition: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported scheduler schema version {version}")
            }
            Self::TooLarge(size) => {
                write!(formatter, "scheduler payload is too large: {size} bytes")
            }
            Self::BudgetExceeded {
                agent_id,
                dimension,
                limit,
                observed,
            } => write!(
                formatter,
                "agent {agent_id} exceeded {dimension:?} budget {limit} with {observed}"
            ),
            Self::OwnershipConflict {
                resource,
                owner,
                requester,
            } => write!(
                formatter,
                "resource {resource:?} is owned by {owner}, not requester {requester}"
            ),
            Self::OwnershipNotHeld { resource, owner } => {
                write!(
                    formatter,
                    "agent {owner} does not own resource {resource:?}"
                )
            }
        }
    }
}

impl std::error::Error for SchedulerError {}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchedulerState {
    budgets: BTreeMap<AgentId, BudgetState>,
    health: BTreeMap<AgentId, HealthState>,
    ownership: BTreeMap<String, AgentId>,
    parent_by_child: BTreeMap<AgentId, AgentId>,
    children_by_parent: BTreeMap<AgentId, Vec<AgentId>>,
}

impl SchedulerState {
    pub fn budget(&self, agent_id: AgentId) -> Option<BudgetState> {
        self.budgets.get(&agent_id).copied()
    }

    pub fn health(&self, agent_id: AgentId) -> HealthState {
        self.health.get(&agent_id).copied().unwrap_or_default()
    }

    pub fn budgets(&self) -> &BTreeMap<AgentId, BudgetState> {
        &self.budgets
    }

    pub fn health_by_agent(&self) -> &BTreeMap<AgentId, HealthState> {
        &self.health
    }

    pub fn owner(&self, resource: &str) -> Option<AgentId> {
        canonical_resource(resource).and_then(|resource| self.ownership.get(&resource).copied())
    }

    pub fn ownership(&self) -> &BTreeMap<String, AgentId> {
        &self.ownership
    }

    pub fn authorize_ownership(
        &self,
        agent_id: AgentId,
        resource: &str,
        access: OwnershipAccess,
    ) -> Result<(), OwnershipError> {
        let resource = canonical_resource(resource).ok_or(OwnershipError::Invalid(
            "resource must be a canonical identity",
        ))?;
        if access == OwnershipAccess::Read {
            return Ok(());
        }
        let mut requester_holds = false;
        for (claimed, owner) in &self.ownership {
            if !resources_overlap(claimed, &resource) {
                continue;
            }
            if *owner == agent_id {
                requester_holds = true;
            } else {
                return Err(OwnershipError::Conflict {
                    agent_id,
                    resource: resource.clone(),
                    owner: *owner,
                });
            }
        }
        if requester_holds {
            Ok(())
        } else {
            Err(OwnershipError::Unowned { agent_id, resource })
        }
    }

    pub fn parent_of(&self, child_id: AgentId) -> Option<AgentId> {
        self.parent_by_child.get(&child_id).copied()
    }

    pub fn children_of(&self, parent_id: AgentId) -> Vec<AgentId> {
        self.children_by_parent
            .get(&parent_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn apply(&mut self, transition: SchedulerTransition) -> Result<(), SchedulerError> {
        match transition {
            SchedulerTransition::BudgetConfigured { agent_id, limits } => {
                let usage = self
                    .budget(agent_id)
                    .map_or(BudgetUsage::default(), |state| state.usage);
                let candidate = BudgetState { limits, usage };
                candidate.check(BudgetUsage::default(), agent_id)?;
                self.budgets.insert(agent_id, candidate);
            }
            SchedulerTransition::UsageRecorded { agent_id, delta } => {
                let state = self
                    .budgets
                    .entry(agent_id)
                    .or_insert_with(|| BudgetState::new(BudgetLimits::default()));
                state.check(delta, agent_id)?;
                state.usage = state.usage.saturating_add(delta);
            }
            SchedulerTransition::HealthSignaled { agent_id, signal } => {
                let state = self.health.entry(agent_id).or_default();
                *state = state.apply_signal(signal);
            }
            SchedulerTransition::OwnershipClaimed { agent_id, resource } => {
                let resource = canonical_scheduler_resource(&resource)?;
                for (claimed, owner) in &self.ownership {
                    if *owner != agent_id && resources_overlap(claimed, &resource) {
                        return Err(SchedulerError::OwnershipConflict {
                            resource,
                            owner: *owner,
                            requester: agent_id,
                        });
                    }
                }
                self.ownership.insert(resource, agent_id);
            }
            SchedulerTransition::OwnershipReleased { agent_id, resource } => {
                let resource = canonical_scheduler_resource(&resource)?;
                match self.owner(&resource) {
                    Some(owner) if owner == agent_id => {
                        self.ownership.remove(&resource);
                    }
                    Some(_) | None => {
                        return Err(SchedulerError::OwnershipNotHeld {
                            resource,
                            owner: agent_id,
                        });
                    }
                }
            }
            SchedulerTransition::ChildSpawned {
                parent_id,
                child_id,
            } => {
                if parent_id == child_id {
                    return Err(SchedulerError::Invalid("an agent cannot be its own child"));
                }
                if self.parent_by_child.contains_key(&child_id) {
                    return Err(SchedulerError::Invalid("child already has a parent"));
                }
                self.parent_by_child.insert(child_id, parent_id);
                self.children_by_parent
                    .entry(parent_id)
                    .or_default()
                    .push(child_id);
            }
            SchedulerTransition::BudgetTransferred {
                from_agent,
                to_agent,
                limits,
            } => {
                if from_agent == to_agent {
                    return Err(SchedulerError::Invalid(
                        "budget cannot be transferred to the same agent",
                    ));
                }
                let source = self.budget(from_agent).ok_or(SchedulerError::Invalid(
                    "source agent has no configured budget",
                ))?;
                let target = self
                    .budget(to_agent)
                    .unwrap_or_else(|| BudgetState::new(BudgetLimits::default()));
                let (source_limits, target_limits) =
                    transfer_limits(source.limits, target.limits, limits)?;
                let source_candidate = BudgetState {
                    limits: source_limits,
                    usage: source.usage,
                };
                source_candidate.check(BudgetUsage::default(), from_agent)?;
                let target_candidate = BudgetState {
                    limits: target_limits,
                    usage: target.usage,
                };
                target_candidate.check(BudgetUsage::default(), to_agent)?;
                self.budgets.insert(from_agent, source_candidate);
                self.budgets.insert(to_agent, target_candidate);
            }
        }
        Ok(())
    }

    pub fn from_events(events: &[Event]) -> Result<Self, SchedulerError> {
        let agent_ids = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::AgentCreated { agent } => Some(agent.id),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut state = Self::default();
        for event in events {
            if let EventKind::SchedulerTransition { version, payload } = &event.kind {
                let transition = decode_transition(*version, payload)?;
                validate_agent_references(&transition, &agent_ids)?;
                state.apply(transition)?;
            }
        }
        Ok(state)
    }
}

impl ResourceOwnershipPolicy for SchedulerState {
    fn authorize(
        &self,
        agent_id: AgentId,
        resource: &str,
        access: OwnershipAccess,
    ) -> Result<(), OwnershipError> {
        self.authorize_ownership(agent_id, resource, access)
    }
}

fn validate_agent_references(
    transition: &SchedulerTransition,
    agent_ids: &BTreeSet<AgentId>,
) -> Result<(), SchedulerError> {
    let references = match transition {
        SchedulerTransition::BudgetConfigured { agent_id, .. }
        | SchedulerTransition::UsageRecorded { agent_id, .. }
        | SchedulerTransition::HealthSignaled { agent_id, .. }
        | SchedulerTransition::OwnershipClaimed { agent_id, .. }
        | SchedulerTransition::OwnershipReleased { agent_id, .. } => {
            vec![*agent_id]
        }
        SchedulerTransition::ChildSpawned {
            parent_id,
            child_id,
        } => vec![*parent_id, *child_id],
        SchedulerTransition::BudgetTransferred {
            from_agent,
            to_agent,
            ..
        } => vec![*from_agent, *to_agent],
    };
    if references
        .iter()
        .all(|agent_id| agent_ids.contains(agent_id))
    {
        Ok(())
    } else {
        Err(SchedulerError::Invalid(
            "scheduler transition references an unknown agent",
        ))
    }
}

fn transfer_limits(
    source: BudgetLimits,
    target: BudgetLimits,
    amount: BudgetLimits,
) -> Result<(BudgetLimits, BudgetLimits), SchedulerError> {
    let (source_tokens, target_tokens) =
        transfer_dimension(source.max_tokens, target.max_tokens, amount.max_tokens)?;
    let (source_money, target_money) = transfer_dimension(
        source.max_money_micros,
        target.max_money_micros,
        amount.max_money_micros,
    )?;
    let (source_wall, target_wall) = transfer_dimension(
        source.max_wall_clock_ms,
        target.max_wall_clock_ms,
        amount.max_wall_clock_ms,
    )?;
    let (source_tools, target_tools) = transfer_dimension(
        source.max_tool_calls,
        target.max_tool_calls,
        amount.max_tool_calls,
    )?;
    let (source_children, target_children) = transfer_dimension(
        source.max_child_agents,
        target.max_child_agents,
        amount.max_child_agents,
    )?;
    let (source_context, target_context) = transfer_dimension(
        source.max_context_tokens,
        target.max_context_tokens,
        amount.max_context_tokens,
    )?;
    Ok((
        BudgetLimits {
            max_tokens: source_tokens,
            max_money_micros: source_money,
            max_wall_clock_ms: source_wall,
            max_tool_calls: source_tools,
            max_child_agents: source_children,
            max_context_tokens: source_context,
        },
        BudgetLimits {
            max_tokens: target_tokens,
            max_money_micros: target_money,
            max_wall_clock_ms: target_wall,
            max_tool_calls: target_tools,
            max_child_agents: target_children,
            max_context_tokens: target_context,
        },
    ))
}

fn transfer_dimension(
    source: Option<u64>,
    target: Option<u64>,
    amount: Option<u64>,
) -> Result<(Option<u64>, Option<u64>), SchedulerError> {
    let Some(amount) = amount else {
        return Ok((source, target));
    };
    if amount == 0 {
        return Ok((source, target));
    }
    if source.is_none() {
        return Err(SchedulerError::Invalid(
            "unlimited budget dimensions cannot transfer finite capacity",
        ));
    }
    let source = source
        .map(|limit| {
            limit.checked_sub(amount).ok_or(SchedulerError::Invalid(
                "budget transfer exceeds source limit",
            ))
        })
        .transpose()?;
    let target = target
        .map(|limit| {
            limit.checked_add(amount).ok_or(SchedulerError::Invalid(
                "budget transfer exceeds target limit",
            ))
        })
        .transpose()?;
    Ok((source, target))
}

fn encode_limits(bytes: &mut Vec<u8>, limits: BudgetLimits) {
    for value in [
        limits.max_tokens,
        limits.max_money_micros,
        limits.max_wall_clock_ms,
        limits.max_tool_calls,
        limits.max_child_agents,
        limits.max_context_tokens,
    ] {
        match value {
            Some(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            None => bytes.push(0),
        }
    }
}

fn decode_limits(cursor: &mut Cursor<'_>) -> Result<BudgetLimits, SchedulerError> {
    let mut values = [None; 6];
    for value in &mut values {
        *value = match cursor.u8()? {
            0 => None,
            1 => Some(cursor.u64()?),
            _ => return Err(SchedulerError::Invalid("unknown budget limit tag")),
        };
    }
    Ok(BudgetLimits {
        max_tokens: values[0],
        max_money_micros: values[1],
        max_wall_clock_ms: values[2],
        max_tool_calls: values[3],
        max_child_agents: values[4],
        max_context_tokens: values[5],
    })
}

pub fn encode_transition(transition: &SchedulerTransition) -> Result<Vec<u8>, SchedulerError> {
    let mut bytes = Vec::with_capacity(64);
    match transition {
        SchedulerTransition::BudgetConfigured { agent_id, limits } => {
            bytes.push(0);
            bytes.extend_from_slice(&agent_id.value().to_le_bytes());
            for value in [
                limits.max_tokens,
                limits.max_money_micros,
                limits.max_wall_clock_ms,
                limits.max_tool_calls,
                limits.max_child_agents,
                limits.max_context_tokens,
            ] {
                match value {
                    Some(value) => {
                        bytes.push(1);
                        bytes.extend_from_slice(&value.to_le_bytes());
                    }
                    None => bytes.push(0),
                }
            }
        }
        SchedulerTransition::UsageRecorded { agent_id, delta } => {
            bytes.push(1);
            bytes.extend_from_slice(&agent_id.value().to_le_bytes());
            encode_usage(&mut bytes, *delta);
        }
        SchedulerTransition::HealthSignaled { agent_id, signal } => {
            bytes.push(2);
            bytes.extend_from_slice(&agent_id.value().to_le_bytes());
            bytes.push(encode_signal(*signal));
        }
        SchedulerTransition::OwnershipClaimed { agent_id, resource } => {
            bytes.push(3);
            bytes.extend_from_slice(&agent_id.value().to_le_bytes());
            encode_resource(&mut bytes, resource)?;
        }
        SchedulerTransition::OwnershipReleased { agent_id, resource } => {
            bytes.push(4);
            bytes.extend_from_slice(&agent_id.value().to_le_bytes());
            encode_resource(&mut bytes, resource)?;
        }
        SchedulerTransition::ChildSpawned {
            parent_id,
            child_id,
        } => {
            bytes.push(5);
            bytes.extend_from_slice(&parent_id.value().to_le_bytes());
            bytes.extend_from_slice(&child_id.value().to_le_bytes());
        }
        SchedulerTransition::BudgetTransferred {
            from_agent,
            to_agent,
            limits,
        } => {
            bytes.push(6);
            bytes.extend_from_slice(&from_agent.value().to_le_bytes());
            bytes.extend_from_slice(&to_agent.value().to_le_bytes());
            encode_limits(&mut bytes, *limits);
        }
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(SchedulerError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

pub fn decode_transition(
    version: u16,
    bytes: &[u8],
) -> Result<SchedulerTransition, SchedulerError> {
    if version != SCHEDULER_SCHEMA_VERSION {
        return Err(SchedulerError::UnsupportedVersion(version));
    }
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(SchedulerError::TooLarge(bytes.len()));
    }
    let mut cursor = Cursor { bytes, offset: 0 };
    let transition = match cursor.u8()? {
        0 => {
            let agent_id = AgentId::from_u64(cursor.u64()?);
            let mut values = [None; 6];
            for value in &mut values {
                *value = match cursor.u8()? {
                    0 => None,
                    1 => Some(cursor.u64()?),
                    _ => return Err(SchedulerError::Invalid("unknown budget limit tag")),
                };
            }
            SchedulerTransition::BudgetConfigured {
                agent_id,
                limits: BudgetLimits {
                    max_tokens: values[0],
                    max_money_micros: values[1],
                    max_wall_clock_ms: values[2],
                    max_tool_calls: values[3],
                    max_child_agents: values[4],
                    max_context_tokens: values[5],
                },
            }
        }
        1 => SchedulerTransition::UsageRecorded {
            agent_id: AgentId::from_u64(cursor.u64()?),
            delta: decode_usage(&mut cursor)?,
        },
        2 => SchedulerTransition::HealthSignaled {
            agent_id: AgentId::from_u64(cursor.u64()?),
            signal: decode_signal(cursor.u8()?)?,
        },
        3 => SchedulerTransition::OwnershipClaimed {
            agent_id: AgentId::from_u64(cursor.u64()?),
            resource: cursor.resource()?,
        },
        4 => SchedulerTransition::OwnershipReleased {
            agent_id: AgentId::from_u64(cursor.u64()?),
            resource: cursor.resource()?,
        },
        5 => SchedulerTransition::ChildSpawned {
            parent_id: AgentId::from_u64(cursor.u64()?),
            child_id: AgentId::from_u64(cursor.u64()?),
        },
        6 => SchedulerTransition::BudgetTransferred {
            from_agent: AgentId::from_u64(cursor.u64()?),
            to_agent: AgentId::from_u64(cursor.u64()?),
            limits: decode_limits(&mut cursor)?,
        },
        _ => return Err(SchedulerError::Invalid("unknown scheduler transition tag")),
    };
    cursor.finish()?;
    Ok(transition)
}

fn encode_usage(bytes: &mut Vec<u8>, usage: BudgetUsage) {
    for value in [
        usage.tokens,
        usage.money_micros,
        usage.wall_clock_ms,
        usage.tool_calls,
        usage.child_agents,
        usage.context_tokens,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn decode_usage(cursor: &mut Cursor<'_>) -> Result<BudgetUsage, SchedulerError> {
    Ok(BudgetUsage {
        tokens: cursor.u64()?,
        money_micros: cursor.u64()?,
        wall_clock_ms: cursor.u64()?,
        tool_calls: cursor.u64()?,
        child_agents: cursor.u64()?,
        context_tokens: cursor.u64()?,
    })
}

fn encode_signal(signal: HealthSignal) -> u8 {
    match signal {
        HealthSignal::Failure => 0,
        HealthSignal::ToolError => 1,
        HealthSignal::NoProgress => 2,
        HealthSignal::ContextPressure => 3,
        HealthSignal::BudgetPressure => 4,
        HealthSignal::AssumptionConflict => 5,
        HealthSignal::VerificationFailure => 6,
        HealthSignal::DependencyInvalidation => 7,
        HealthSignal::Progress => 8,
        HealthSignal::FailureResolved => 9,
        HealthSignal::ToolErrorResolved => 10,
        HealthSignal::NoProgressResolved => 11,
        HealthSignal::ContextPressureResolved => 12,
        HealthSignal::BudgetPressureResolved => 13,
        HealthSignal::AssumptionConflictResolved => 14,
        HealthSignal::VerificationFailureResolved => 15,
        HealthSignal::DependencyInvalidationResolved => 16,
    }
}

fn validate_resource(resource: &str) -> Result<(), SchedulerError> {
    if resource.trim().is_empty() {
        return Err(SchedulerError::Invalid(
            "ownership resource must not be empty",
        ));
    }
    if resource.len() > MAX_RESOURCE_BYTES {
        return Err(SchedulerError::TooLarge(resource.len()));
    }
    Ok(())
}

fn canonical_scheduler_resource(resource: &str) -> Result<String, SchedulerError> {
    validate_resource(resource)?;
    canonical_resource(resource).ok_or(SchedulerError::Invalid(
        "ownership resource must not escape its relative root",
    ))
}

fn encode_resource(bytes: &mut Vec<u8>, resource: &str) -> Result<(), SchedulerError> {
    let resource = canonical_scheduler_resource(resource)?;
    let length =
        u32::try_from(resource.len()).map_err(|_| SchedulerError::TooLarge(resource.len()))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(resource.as_bytes());
    Ok(())
}

fn decode_signal(tag: u8) -> Result<HealthSignal, SchedulerError> {
    match tag {
        0 => Ok(HealthSignal::Failure),
        1 => Ok(HealthSignal::ToolError),
        2 => Ok(HealthSignal::NoProgress),
        3 => Ok(HealthSignal::ContextPressure),
        4 => Ok(HealthSignal::BudgetPressure),
        5 => Ok(HealthSignal::AssumptionConflict),
        6 => Ok(HealthSignal::VerificationFailure),
        7 => Ok(HealthSignal::DependencyInvalidation),
        8 => Ok(HealthSignal::Progress),
        9 => Ok(HealthSignal::FailureResolved),
        10 => Ok(HealthSignal::ToolErrorResolved),
        11 => Ok(HealthSignal::NoProgressResolved),
        12 => Ok(HealthSignal::ContextPressureResolved),
        13 => Ok(HealthSignal::BudgetPressureResolved),
        14 => Ok(HealthSignal::AssumptionConflictResolved),
        15 => Ok(HealthSignal::VerificationFailureResolved),
        16 => Ok(HealthSignal::DependencyInvalidationResolved),
        _ => Err(SchedulerError::Invalid("unknown health signal")),
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn take(&mut self, length: usize) -> Result<&[u8], SchedulerError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(SchedulerError::Invalid("payload offset overflow"))?;
        if end > self.bytes.len() {
            return Err(SchedulerError::Invalid("scheduler payload is truncated"));
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, SchedulerError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, SchedulerError> {
        let mut value = [0; 8];
        value.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(value))
    }

    fn resource(&mut self) -> Result<String, SchedulerError> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| SchedulerError::Invalid("resource length overflow"))?;
        if length > MAX_RESOURCE_BYTES {
            return Err(SchedulerError::TooLarge(length));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| SchedulerError::Invalid("ownership resource is not UTF-8"))
    }

    fn u32(&mut self) -> Result<u32, SchedulerError> {
        let mut value = [0; 4];
        value.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(value))
    }

    fn finish(self) -> Result<(), SchedulerError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(SchedulerError::Invalid(
                "scheduler payload has trailing bytes",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_kernel::{AgentIdentity, Event, EventKind, ModelClass, ModelRef, RunId};

    #[test]
    fn budget_usage_is_rejected_before_overrun() {
        let agent_id = AgentId::from_u64(7);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id,
                limits: BudgetLimits {
                    max_tokens: Some(10),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        state
            .apply(SchedulerTransition::UsageRecorded {
                agent_id,
                delta: BudgetUsage {
                    tokens: 10,
                    ..BudgetUsage::default()
                },
            })
            .unwrap();
        assert!(matches!(
            state.apply(SchedulerTransition::UsageRecorded {
                agent_id,
                delta: BudgetUsage {
                    tokens: 1,
                    ..BudgetUsage::default()
                },
            }),
            Err(SchedulerError::BudgetExceeded { .. })
        ));
        assert!(matches!(
            state.apply(SchedulerTransition::BudgetConfigured {
                agent_id,
                limits: BudgetLimits {
                    max_tokens: Some(9),
                    ..BudgetLimits::default()
                },
            }),
            Err(SchedulerError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn budget_transfers_move_configured_capacity_without_moving_usage() {
        let source = AgentId::from_u64(7);
        let target = AgentId::from_u64(8);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: source,
                limits: BudgetLimits {
                    max_tokens: Some(100),
                    max_tool_calls: Some(4),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: target,
                limits: BudgetLimits {
                    max_tokens: Some(10),
                    max_tool_calls: Some(1),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        state
            .apply(SchedulerTransition::UsageRecorded {
                agent_id: source,
                delta: BudgetUsage {
                    tokens: 20,
                    ..BudgetUsage::default()
                },
            })
            .unwrap();
        let transition = SchedulerTransition::BudgetTransferred {
            from_agent: source,
            to_agent: target,
            limits: BudgetLimits {
                max_tokens: Some(25),
                max_tool_calls: Some(2),
                ..BudgetLimits::default()
            },
        };
        let payload = encode_transition(&transition).unwrap();
        assert_eq!(
            decode_transition(SCHEDULER_SCHEMA_VERSION, &payload).unwrap(),
            transition
        );
        state.apply(transition).unwrap();
        assert_eq!(state.budget(source).unwrap().limits.max_tokens, Some(75));
        assert_eq!(state.budget(source).unwrap().limits.max_tool_calls, Some(2));
        assert_eq!(state.budget(source).unwrap().usage.tokens, 20);
        assert_eq!(state.budget(target).unwrap().limits.max_tokens, Some(35));
        assert_eq!(state.budget(target).unwrap().limits.max_tool_calls, Some(3));
    }

    #[test]
    fn budget_transfer_rejects_reducing_a_source_below_existing_usage() {
        let source = AgentId::from_u64(7);
        let target = AgentId::from_u64(8);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: source,
                limits: BudgetLimits {
                    max_tokens: Some(10),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: target,
                limits: BudgetLimits::default(),
            })
            .unwrap();
        state
            .apply(SchedulerTransition::UsageRecorded {
                agent_id: source,
                delta: BudgetUsage {
                    tokens: 8,
                    ..BudgetUsage::default()
                },
            })
            .unwrap();
        assert!(matches!(
            state.apply(SchedulerTransition::BudgetTransferred {
                from_agent: source,
                to_agent: target,
                limits: BudgetLimits {
                    max_tokens: Some(3),
                    ..BudgetLimits::default()
                },
            }),
            Err(SchedulerError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn budget_transfer_rejects_unlimited_or_unconfigured_dimensions() {
        let source = AgentId::from_u64(7);
        let target = AgentId::from_u64(8);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: source,
                limits: BudgetLimits::default(),
            })
            .unwrap();
        state
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: target,
                limits: BudgetLimits {
                    max_tokens: Some(5),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        assert!(matches!(
            state.apply(SchedulerTransition::BudgetTransferred {
                from_agent: source,
                to_agent: target,
                limits: BudgetLimits {
                    max_tokens: Some(1),
                    ..BudgetLimits::default()
                },
            }),
            Err(SchedulerError::Invalid(_))
        ));

        let mut finite_source = SchedulerState::default();
        finite_source
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: source,
                limits: BudgetLimits {
                    max_tokens: Some(5),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        finite_source
            .apply(SchedulerTransition::BudgetConfigured {
                agent_id: target,
                limits: BudgetLimits::default(),
            })
            .unwrap();
        finite_source
            .apply(SchedulerTransition::BudgetTransferred {
                from_agent: source,
                to_agent: target,
                limits: BudgetLimits {
                    max_tokens: Some(1),
                    ..BudgetLimits::default()
                },
            })
            .unwrap();
        assert_eq!(
            finite_source.budget(source).unwrap().limits.max_tokens,
            Some(4)
        );
        assert_eq!(
            finite_source.budget(target).unwrap().limits.max_tokens,
            None
        );
    }

    #[test]
    fn cache_affinity_ranking_uses_only_explicit_observations() {
        let warm = ModelRef::new("provider-a", "warm", ModelClass::Strong);
        let cold = ModelRef::new("provider-b", "cold", ModelClass::Cheap);
        let ranked = rank_cache_aware_candidates(
            vec![
                CacheAwareRouteCandidate {
                    model: warm.clone(),
                    estimated_cost_micros: 100,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: None,
                },
                CacheAwareRouteCandidate {
                    model: cold.clone(),
                    estimated_cost_micros: 20,
                    observed_cached_tokens: None,
                    observed_at_ms: None,
                },
            ],
            CacheRoutingPolicy {
                prefer_warm_cache: true,
                cached_token_value_micros: 2,
                max_observation_age_ms: None,
            },
        );
        assert_eq!(ranked[0].model, warm);
        assert_eq!(ranked[0].effective_cost_micros, 0);
        assert_eq!(ranked[1].observed_cached_tokens, None);

        let cost_only = rank_cache_aware_candidates(
            ranked.iter().map(|route| CacheAwareRouteCandidate {
                model: route.model.clone(),
                estimated_cost_micros: route.estimated_cost_micros,
                observed_cached_tokens: route.observed_cached_tokens,
                observed_at_ms: None,
            }),
            CacheRoutingPolicy {
                prefer_warm_cache: false,
                cached_token_value_micros: 2,
                max_observation_age_ms: None,
            },
        );
        assert_eq!(cost_only[0].model, cold);

        let explicit_zero = rank_cache_aware_candidates(
            [CacheAwareRouteCandidate {
                model: warm,
                estimated_cost_micros: 1,
                observed_cached_tokens: Some(0),
                observed_at_ms: None,
            }],
            CacheRoutingPolicy::default(),
        );
        assert_eq!(explicit_zero[0].observed_cached_tokens, Some(0));
        assert_eq!(explicit_zero[0].effective_cost_micros, 1);
    }

    #[test]
    fn cache_affinity_freshness_never_claims_stale_or_unverifiable_savings() {
        let fresh = ModelRef::new("provider-a", "fresh", ModelClass::Cheap);
        let stale = ModelRef::new("provider-b", "stale", ModelClass::Cheap);
        let future = ModelRef::new("provider-c", "future", ModelClass::Cheap);
        let policy = CacheRoutingPolicy {
            prefer_warm_cache: true,
            cached_token_value_micros: 2,
            max_observation_age_ms: Some(100),
        };

        let ranked = rank_cache_aware_candidates_at(
            [
                CacheAwareRouteCandidate {
                    model: fresh.clone(),
                    estimated_cost_micros: 100,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: Some(950),
                },
                CacheAwareRouteCandidate {
                    model: stale.clone(),
                    estimated_cost_micros: 10,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: Some(899),
                },
                CacheAwareRouteCandidate {
                    model: future.clone(),
                    estimated_cost_micros: 11,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: Some(1001),
                },
            ],
            policy,
            1000,
        );
        let fresh_route = ranked.iter().find(|route| route.model == fresh).unwrap();
        let stale_route = ranked.iter().find(|route| route.model == stale).unwrap();
        let future_route = ranked.iter().find(|route| route.model == future).unwrap();
        assert!(fresh_route.cache_observation_fresh);
        assert_eq!(fresh_route.effective_cost_micros, 0);
        assert!(!stale_route.cache_observation_fresh);
        assert_eq!(stale_route.effective_cost_micros, 10);
        assert!(!future_route.cache_observation_fresh);
        assert_eq!(future_route.effective_cost_micros, 11);

        let without_clock = rank_cache_aware_candidates(
            [CacheAwareRouteCandidate {
                model: fresh,
                estimated_cost_micros: 100,
                observed_cached_tokens: Some(50),
                observed_at_ms: Some(950),
            }],
            policy,
        );
        assert!(!without_clock[0].cache_observation_fresh);
        assert_eq!(without_clock[0].effective_cost_micros, 100);

        let disabled = rank_cache_aware_candidates_at(
            [
                CacheAwareRouteCandidate {
                    model: fresh_route.model.clone(),
                    estimated_cost_micros: 10,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: Some(950),
                },
                CacheAwareRouteCandidate {
                    model: stale_route.model.clone(),
                    estimated_cost_micros: 10,
                    observed_cached_tokens: Some(1),
                    observed_at_ms: Some(899),
                },
            ],
            CacheRoutingPolicy {
                prefer_warm_cache: false,
                cached_token_value_micros: 2,
                max_observation_age_ms: Some(100),
            },
            1000,
        );
        assert_eq!(disabled[0].model.provider, "provider-a");

        let stale_tie = rank_cache_aware_candidates_at(
            [
                CacheAwareRouteCandidate {
                    model: ModelRef::new("provider-z", "warm", ModelClass::Cheap),
                    estimated_cost_micros: 10,
                    observed_cached_tokens: Some(50),
                    observed_at_ms: Some(0),
                },
                CacheAwareRouteCandidate {
                    model: ModelRef::new("provider-a", "cold", ModelClass::Cheap),
                    estimated_cost_micros: 10,
                    observed_cached_tokens: None,
                    observed_at_ms: None,
                },
            ],
            CacheRoutingPolicy {
                prefer_warm_cache: true,
                cached_token_value_micros: 1,
                max_observation_age_ms: Some(1),
            },
            1000,
        );
        assert_eq!(stale_tie[0].model.provider, "provider-a");
    }

    #[test]
    fn supervision_promotes_only_after_policy_threshold_to_deterministic_stronger_model() {
        let current = ModelRef::new("provider-a", "cheap", ModelClass::Cheap);
        let strong_a = ModelRef::new("provider-z", "strong-a", ModelClass::Strong);
        let strong_b = ModelRef::new("provider-a", "strong-b", ModelClass::Strong);
        let action = choose_supervision_action(
            HealthState {
                status: HealthStatus::Degraded,
                consecutive_failures: 2,
                ..HealthState::default()
            },
            &current,
            true,
            false,
            &[strong_a, strong_b.clone()],
            SupervisionPolicy {
                promote_after_failures: Some(2),
                pause_when_blocked: true,
            },
        )
        .expect("policy should be valid");
        assert_eq!(action, SupervisionAction::Promote(strong_b));
    }

    #[test]
    fn supervision_respects_pins_and_pauses_blocked_agents_without_candidates() {
        let current = ModelRef::new("provider", "strong", ModelClass::Strong);
        let policy = SupervisionPolicy {
            promote_after_failures: Some(1),
            pause_when_blocked: true,
        };
        let health = HealthState {
            status: HealthStatus::Blocked,
            consecutive_failures: 3,
            ..HealthState::default()
        };
        assert_eq!(
            choose_supervision_action(health, &current, true, true, &[], policy)
                .expect("policy should be valid"),
            SupervisionAction::Pause
        );
        assert_eq!(
            choose_supervision_action(health, &current, false, false, &[], policy)
                .expect("policy should be valid"),
            SupervisionAction::Pause
        );
    }

    #[test]
    fn supervision_rejects_zero_promotion_threshold() {
        let current = ModelRef::new("provider", "cheap", ModelClass::Cheap);
        assert!(matches!(
            choose_supervision_action(
                HealthState::default(),
                &current,
                true,
                false,
                &[],
                SupervisionPolicy {
                    promote_after_failures: Some(0),
                    pause_when_blocked: true,
                },
            ),
            Err(SchedulerError::Invalid(_))
        ));
    }

    #[test]
    fn health_thresholds_are_deterministic_and_replayable() {
        let agent_id = AgentId::from_u64(7);
        let run_id = RunId::from_u64(1);
        let mut events = vec![
            Event::new(run_id, EventKind::RunCreated { run_id }),
            Event::new(
                run_id,
                EventKind::AgentCreated {
                    agent: AgentIdentity {
                        id: agent_id,
                        name: "worker".to_owned(),
                        mission: "test health".to_owned(),
                        model: ModelRef::new("mock", "cheap", ModelClass::Cheap),
                    },
                },
            ),
        ];
        for _ in 0..3 {
            events.push(Event::new(
                run_id,
                EventKind::SchedulerTransition {
                    version: SCHEDULER_SCHEMA_VERSION,
                    payload: encode_transition(&SchedulerTransition::HealthSignaled {
                        agent_id,
                        signal: HealthSignal::Failure,
                    })
                    .unwrap(),
                },
            ));
        }
        let state = SchedulerState::from_events(&events).unwrap();
        assert_eq!(state.health(agent_id).status, HealthStatus::Blocked);
    }

    #[test]
    fn health_history_remains_while_resolution_recovers_current_status() {
        let agent_id = AgentId::from_u64(7);
        let mut state = SchedulerState::default();
        for _ in 0..3 {
            state
                .apply(SchedulerTransition::HealthSignaled {
                    agent_id,
                    signal: HealthSignal::Failure,
                })
                .unwrap();
        }
        assert_eq!(state.health(agent_id).status, HealthStatus::Blocked);
        assert_eq!(state.health(agent_id).consecutive_failures, 3);
        assert_eq!(state.health(agent_id).active_failures, 3);
        state
            .apply(SchedulerTransition::HealthSignaled {
                agent_id,
                signal: HealthSignal::Progress,
            })
            .unwrap();
        assert_eq!(state.health(agent_id).status, HealthStatus::Healthy);
        assert_eq!(state.health(agent_id).consecutive_failures, 0);
        assert_eq!(state.health(agent_id).active_failures, 0);

        for _ in 0..2 {
            state
                .apply(SchedulerTransition::HealthSignaled {
                    agent_id,
                    signal: HealthSignal::AssumptionConflict,
                })
                .unwrap();
        }
        assert_eq!(state.health(agent_id).status, HealthStatus::Blocked);
        state
            .apply(SchedulerTransition::HealthSignaled {
                agent_id,
                signal: HealthSignal::AssumptionConflictResolved,
            })
            .unwrap();
        state
            .apply(SchedulerTransition::HealthSignaled {
                agent_id,
                signal: HealthSignal::AssumptionConflictResolved,
            })
            .unwrap();
        let health = state.health(agent_id);
        assert_eq!(health.status, HealthStatus::Healthy);
        assert_eq!(health.assumption_conflicts, 2);
        assert_eq!(health.active_assumption_conflicts, 0);
    }

    #[test]
    fn transition_codec_rejects_trailing_bytes() {
        let mut payload = encode_transition(&SchedulerTransition::HealthSignaled {
            agent_id: AgentId::from_u64(7),
            signal: HealthSignal::Progress,
        })
        .unwrap();
        payload.push(1);
        assert!(matches!(
            decode_transition(SCHEDULER_SCHEMA_VERSION, &payload),
            Err(SchedulerError::Invalid(_))
        ));
    }

    #[test]
    fn ownership_claims_are_replayable_and_conflict_checked() {
        let first = AgentId::from_u64(7);
        let second = AgentId::from_u64(8);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::OwnershipClaimed {
                agent_id: first,
                resource: "repo:api".to_owned(),
            })
            .unwrap();
        assert_eq!(state.owner("repo:api"), Some(first));
        assert!(matches!(
            state.apply(SchedulerTransition::OwnershipClaimed {
                agent_id: second,
                resource: "repo:api".to_owned(),
            }),
            Err(SchedulerError::OwnershipConflict { .. })
        ));
        assert!(matches!(
            state.apply(SchedulerTransition::OwnershipReleased {
                agent_id: second,
                resource: "repo:api".to_owned(),
            }),
            Err(SchedulerError::OwnershipNotHeld { .. })
        ));
        let transition = SchedulerTransition::OwnershipReleased {
            agent_id: first,
            resource: "repo:api".to_owned(),
        };
        let payload = encode_transition(&transition).unwrap();
        assert_eq!(
            decode_transition(SCHEDULER_SCHEMA_VERSION, &payload).unwrap(),
            transition
        );
        state.apply(transition).unwrap();
        assert_eq!(state.owner("repo:api"), None);
    }

    #[test]
    fn ownership_policy_distinguishes_read_access_and_overlapping_writes() {
        let owner = AgentId::from_u64(7);
        let other = AgentId::from_u64(8);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::OwnershipClaimed {
                agent_id: owner,
                resource: "workspace/src".to_owned(),
            })
            .unwrap();
        assert!(
            state
                .authorize_ownership(owner, "workspace/src/lib.rs", OwnershipAccess::Write)
                .is_ok()
        );
        assert!(
            state
                .authorize_ownership(other, "workspace/src/lib.rs", OwnershipAccess::Read)
                .is_ok()
        );
        assert!(matches!(
            state.authorize_ownership(other, "workspace/src/lib.rs", OwnershipAccess::Write),
            Err(OwnershipError::Conflict { .. })
        ));
        assert!(matches!(
            state.apply(SchedulerTransition::OwnershipClaimed {
                agent_id: other,
                resource: "workspace/src/auth".to_owned(),
            }),
            Err(SchedulerError::OwnershipConflict { .. })
        ));
    }

    #[test]
    fn ownership_claims_use_one_canonical_identity_for_replay_and_release() {
        let owner = AgentId::from_u64(7);
        let mut state = SchedulerState::default();
        state
            .apply(SchedulerTransition::OwnershipClaimed {
                agent_id: owner,
                resource: "workspace/src/./auth/../lib.rs".to_owned(),
            })
            .unwrap();
        assert_eq!(state.owner("workspace/src/lib.rs"), Some(owner));
        assert!(matches!(
            state.apply(SchedulerTransition::OwnershipClaimed {
                agent_id: AgentId::from_u64(8),
                resource: "workspace/src/lib.rs".to_owned(),
            }),
            Err(SchedulerError::OwnershipConflict { .. })
        ));
        state
            .apply(SchedulerTransition::OwnershipReleased {
                agent_id: owner,
                resource: "workspace/src/lib.rs".to_owned(),
            })
            .unwrap();
        assert_eq!(state.owner("workspace/src/lib.rs"), None);
        assert!(matches!(
            state.apply(SchedulerTransition::OwnershipClaimed {
                agent_id: owner,
                resource: "../../outside".to_owned(),
            }),
            Err(SchedulerError::Invalid(_))
        ));
    }

    #[test]
    fn child_relationships_are_replayable_and_parent_scoped() {
        let parent_id = AgentId::from_u64(7);
        let child_id = AgentId::from_u64(8);
        let transition = SchedulerTransition::ChildSpawned {
            parent_id,
            child_id,
        };
        let payload = encode_transition(&transition).unwrap();
        let decoded = decode_transition(SCHEDULER_SCHEMA_VERSION, &payload).unwrap();
        let mut state = SchedulerState::default();
        state.apply(decoded).unwrap();
        assert_eq!(state.parent_of(child_id), Some(parent_id));
        assert_eq!(state.children_of(parent_id), vec![child_id]);
        assert!(matches!(
            state.apply(SchedulerTransition::ChildSpawned {
                parent_id,
                child_id,
            }),
            Err(SchedulerError::Invalid("child already has a parent"))
        ));
    }
}
