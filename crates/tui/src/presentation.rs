//! Human-facing presentation of authoritative runtime data.
//!
//! This module is deliberately deterministic. It translates domain values
//! into concise operator language while keeping raw values available to the
//! technical detail panels.

use orynth_assumptions::{AssumptionTransition, decode_transition as decode_assumption};
use orynth_context::{ContextTransition, decode_transition as decode_context};
use orynth_event_store::{AgentStatus, StoredEvent};
use orynth_ipc::{IpcEnvelope, IpcMessage};
use orynth_kernel::{AgentId, EventKind, ModelClass, ModelRef};
use orynth_runtime::conversation::{ConversationSpeaker, ConversationTurn};
use orynth_runtime::{ManagerAgentProjection, RecoveredRun};
use orynth_scheduler::{HealthSignal, HealthStatus, decode_transition as decode_scheduler};
use orynth_security::decode_transition as decode_capability;
use orynth_tool_runtime::{ToolState, ToolTransition, decode_transition as decode_tool};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventSeverity {
    Info,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventPresentation {
    pub title: String,
    pub summary: String,
    pub why: String,
    pub involved: Vec<String>,
    pub kind: String,
    pub severity: EventSeverity,
    pub actor: Option<String>,
    pub related_entities: Vec<String>,
    pub sequence: u64,
    pub occurred_at_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessagePresentation {
    pub route: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub why: String,
}

pub fn agent_label(recovered: &RecoveredRun, agent_id: AgentId) -> String {
    recovered
        .manager
        .agents
        .get(&agent_id)
        .map(|agent| agent.name.clone())
        .unwrap_or_else(|| "Runtime".to_owned())
}

pub fn agent_role(agent: &ManagerAgentProjection) -> String {
    agent
        .specialist
        .as_ref()
        .map(|profile| profile.role.clone())
        .unwrap_or_else(|| "Project Coordinator".to_owned())
}

pub fn model_name(model: &ModelRef) -> String {
    let known = match model.model.as_str() {
        "strong-reasoner-v1" => Some("Sol"),
        "cheap-coder-v2" => Some("Luna"),
        "local-reviewer" => Some("Local Reviewer"),
        _ => None,
    };
    known.map_or_else(|| title_words(&model.model), str::to_owned)
}

pub fn model_class(model: &ModelClass) -> &'static str {
    match model {
        ModelClass::Local => "Local model",
        ModelClass::Cheap => "Cheap model",
        ModelClass::Strong => "Strong model",
        ModelClass::Custom(_) => "Custom model",
    }
}

pub fn agent_status(status: AgentStatus, health: HealthStatus) -> (&'static str, &'static str) {
    let lifecycle = match status {
        AgentStatus::Created => "Starting",
        AgentStatus::Running => "Running",
        AgentStatus::Paused => "Paused",
        AgentStatus::Completed => "Finished",
        AgentStatus::Cancelled => "Cancelled",
        AgentStatus::Failed => "Failed",
    };
    let health = match (status, health) {
        (AgentStatus::Paused, _) => "Paused",
        (AgentStatus::Failed, _) => "Failed",
        (AgentStatus::Cancelled, _) => "Stopped",
        (_, HealthStatus::Healthy) => "Running normally",
        (_, HealthStatus::Degraded) => "Needs attention",
        (_, HealthStatus::Blocked) => "Blocked",
    };
    (lifecycle, health)
}

pub fn health_marker(health: HealthStatus) -> (&'static str, &'static str) {
    match health {
        HealthStatus::Healthy => ("●", "Healthy"),
        HealthStatus::Degraded => ("!", "Needs attention"),
        HealthStatus::Blocked => ("×", "Blocked"),
    }
}

pub fn human_event(recovered: &RecoveredRun, stored: &StoredEvent) -> EventPresentation {
    let kind = event_kind_name(&stored.event.kind).to_owned();
    let mut involved = Vec::new();
    let (title, summary, why) = match &stored.event.kind {
        EventKind::RunCreated { .. } => (
            "Run started".to_owned(),
            "Orynth began coordinating this team.".to_owned(),
            "This is the first durable event for the run.".to_owned(),
        ),
        EventKind::TaskCreated { title, .. } => (
            "Work was defined".to_owned(),
            format!("The team is working on {title}.",),
            "The task gives the agents a shared objective.".to_owned(),
        ),
        EventKind::AgentCreated { agent } => {
            let role = recovered
                .manager
                .agents
                .get(&agent.id)
                .map(agent_role)
                .unwrap_or_else(|| "Team member".to_owned());
            involved.push(agent.name.clone());
            (
                format!("{} joined the team", agent.name),
                format!("{} is responsible for {}.", agent.name, role),
                "The agent is now part of the runtime team.".to_owned(),
            )
        }
        EventKind::ModelRequested { agent_id, model } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} selected a model"),
                format!("{} is powering {name}.", model_name(model)),
                "Model selection changes the effective model without changing the logical agent identity.".to_owned(),
            )
        }
        EventKind::ModelResponseMetadata {
            agent_id,
            resolved_model,
            provider_request_id,
            provider_name,
        } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} received provider metadata"),
                format!(
                    "Resolved model: {}.",
                    resolved_model.as_deref().unwrap_or("not reported")
                ),
                format!(
                    "Provider: {}; request ID: {}.",
                    provider_name.as_deref().unwrap_or("not reported"),
                    provider_request_id.as_deref().unwrap_or("not reported")
                ),
            )
        }
        EventKind::ModelChunkReceived { agent_id, .. } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} received model output"),
                format!("{name} is receiving a response from its model."),
                "Streaming output is recorded as runtime history.".to_owned(),
            )
        }
        EventKind::ModelCompleted { agent_id, .. } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} finished a model response"),
                format!("{name} completed its latest model step."),
                "The response and usage are now part of the recovered projection.".to_owned(),
            )
        }
        EventKind::ModelFinishedWithoutUsage { agent_id } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} finished a model response"),
                format!("{name} completed its latest model step."),
                "The provider did not report token usage.".to_owned(),
            )
        }
        EventKind::ModelTurnCompleted { agent_id, usage } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} finished a response"),
                format!("{name} is ready for another turn."),
                if usage.is_some() {
                    "Provider token usage was recorded.".to_owned()
                } else {
                    "The provider did not report token usage.".to_owned()
                },
            )
        }
        EventKind::ModelTurnOutcome {
            agent_id,
            finish,
            continued,
            unproductive,
        } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} completed a {finish} turn"),
                if *unproductive {
                    format!("{name} produced no visible output; bounded recovery is continuing.")
                } else if *continued {
                    format!("{name} is continuing from the durable runtime state.")
                } else {
                    format!("{name} produced a final response for this turn.")
                },
                "The provider finish state is recorded separately from logical agent completion."
                    .to_owned(),
            )
        }
        EventKind::ModelTurnCancelled { agent_id } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name}'s response was cancelled"),
                "The current response stopped before completion.".to_owned(),
                "The logical agent remains available for another turn.".to_owned(),
            )
        }
        EventKind::ModelTurnFailed { agent_id, message } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name}'s response failed"),
                message.clone(),
                "The logical agent remains available for another turn.".to_owned(),
            )
        }
        EventKind::ModelCancelled { agent_id } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name}'s model response was cancelled"),
                "The response stopped before completion.".to_owned(),
                "Cancellation is recorded so replay preserves the boundary.".to_owned(),
            )
        }
        EventKind::ModelFailed { agent_id, message } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name}'s model response failed"),
                message.clone(),
                "The failure may require attention from the coordinator.".to_owned(),
            )
        }
        EventKind::RunCompleted { .. } => (
            "Run finished".to_owned(),
            "The team completed the run.".to_owned(),
            "No further runtime work is expected.".to_owned(),
        ),
        EventKind::RunCancelled { .. } => (
            "Run cancelled".to_owned(),
            "The run was stopped before completion.".to_owned(),
            "Cancellation is a terminal run decision.".to_owned(),
        ),
        EventKind::RunFailed { message, .. } => (
            "Run failed".to_owned(),
            message.clone(),
            "The run reached a terminal failure state.".to_owned(),
        ),
        EventKind::ArtifactCreated {
            media_type,
            size_bytes,
            ..
        } => (
            "Project artifact recorded".to_owned(),
            format!("A {media_type} artifact ({size_bytes} bytes) is attached to the run."),
            "Artifacts make durable outputs available to recovery.".to_owned(),
        ),
        EventKind::ContextTransition { version, payload } => {
            context_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::CacheObserved {
            model,
            cached_input_tokens,
            ..
        } => (
            "Cache usage observed".to_owned(),
            format!(
                "The run reused {cached_input_tokens} input tokens while using {}.",
                model_name(&ModelRef::new("", model, ModelClass::Custom("".to_owned())))
            ),
            "This is recorded provider telemetry, not an inferred cache hit.".to_owned(),
        ),
        EventKind::AgentMessage { version, payload } => {
            message_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::ConversationTurn { version, payload } => {
            match ConversationTurn::decode(*version, payload) {
                Ok(turn) => {
                    let speaker = match turn.speaker {
                        ConversationSpeaker::User => "You".to_owned(),
                        ConversationSpeaker::Coordinator(agent_id) => {
                            agent_label(recovered, agent_id)
                        }
                    };
                    involved.push(speaker.clone());
                    (
                        format!("{speaker} said"),
                        turn.content,
                        "This turn is recorded in the authoritative run history.".to_owned(),
                    )
                }
                Err(_) => (
                    "Unreadable conversation turn".to_owned(),
                    "The stored conversation payload could not be decoded.".to_owned(),
                    "Inspect the raw event in Advanced Debugger.".to_owned(),
                ),
            }
        }
        EventKind::AssumptionTransition { version, payload } => {
            assumption_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::SchedulerTransition { version, payload } => {
            scheduler_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::AgentPaused { agent_id } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} was paused"),
                format!("{name} is waiting for the runtime to resume it."),
                "A paused agent will not continue work until resumed.".to_owned(),
            )
        }
        EventKind::AgentResumed { agent_id } => {
            let name = agent_label(recovered, *agent_id);
            involved.push(name.clone());
            (
                format!("{name} resumed"),
                format!("{name} can continue its assigned work."),
                "The runtime made the agent eligible to proceed.".to_owned(),
            )
        }
        EventKind::CapabilityTransition { version, payload } => {
            capability_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::ToolTransition { version, payload } => {
            tool_event_presentation(recovered, *version, payload, &mut involved)
        }
        EventKind::FailureMemoryTransition { .. } => (
            "A failure record changed".to_owned(),
            "Orynth updated its bounded record of an attempted approach.".to_owned(),
            "Failure memory helps the coordinator avoid repeating known problems.".to_owned(),
        ),
        EventKind::SpecialistTransition { .. } => (
            "A specialist profile changed".to_owned(),
            "A team's role, scope, or subscription profile changed.".to_owned(),
            "Profiles describe responsibility; they do not grant authority.".to_owned(),
        ),
    };
    let severity = match &stored.event.kind {
        EventKind::RunCompleted { .. }
        | EventKind::ModelCompleted { .. }
        | EventKind::ModelTurnCompleted { .. }
        | EventKind::ModelTurnOutcome { .. }
        | EventKind::AgentResumed { .. } => EventSeverity::Success,
        EventKind::RunFailed { .. }
        | EventKind::ModelFailed { .. }
        | EventKind::ModelTurnFailed { .. }
        | EventKind::RunCancelled { .. } => EventSeverity::Danger,
        EventKind::AssumptionTransition { .. } | EventKind::AgentMessage { .. } => {
            if title.to_ascii_lowercase().contains("conflict") {
                EventSeverity::Warning
            } else {
                EventSeverity::Info
            }
        }
        EventKind::AgentPaused { .. } | EventKind::CapabilityTransition { .. } => {
            EventSeverity::Warning
        }
        _ => EventSeverity::Info,
    };
    EventPresentation {
        title,
        summary,
        why,
        actor: involved.first().cloned(),
        related_entities: involved.clone(),
        involved,
        kind,
        severity,
        sequence: stored.sequence,
        occurred_at_ms: stored.event.occurred_at_ms,
    }
}

pub fn message_presentation(
    recovered: &RecoveredRun,
    message: &IpcEnvelope,
) -> MessagePresentation {
    let sender = agent_label(recovered, message.sender);
    let recipient = agent_label(recovered, message.recipient);
    let route = format!("{sender}  →  {recipient}");
    let (kind, title, body, why) = match &message.payload {
        IpcMessage::Question { subject, why } => (
            "QUESTION",
            subject.clone(),
            why.clone(),
            "A focused question keeps coordination narrow and reviewable.",
        ),
        IpcMessage::Answer {
            subject,
            value,
            evidence,
            ..
        } => (
            "ANSWER",
            subject.clone(),
            format!("{value}\nEvidence: {}", join_or_none(evidence)),
            "The recipient supplied a typed answer.",
        ),
        IpcMessage::Assumption {
            subject,
            normalized_value,
            claim,
            ..
        } => (
            "ASSUMPTION",
            subject.clone(),
            format!("{claim}\nValue: {normalized_value}"),
            "The message records a project claim.",
        ),
        IpcMessage::Decision {
            subject,
            value,
            rationale,
        } => (
            "DECISION",
            subject.clone(),
            format!("{value}\n{rationale}"),
            "A decision gives the team a shared direction.",
        ),
        IpcMessage::Conflict {
            subject,
            left,
            right,
            affected,
        } => (
            "CONFLICT WARNING",
            subject.clone(),
            format!("{left}  VS  {right}\nAffected: {}", join_or_none(affected)),
            "These agents are working with different values for the same project contract.",
        ),
        IpcMessage::Blocked { reason } => (
            "BLOCKED",
            "Work is blocked".to_owned(),
            reason.clone(),
            "The runtime needs a decision or new evidence before this work can continue.",
        ),
        IpcMessage::Progress {
            summary,
            completed_millis,
        } => (
            "PROGRESS",
            "Work progressed".to_owned(),
            format!("{summary}\nReported progress: {completed_millis}/1000"),
            "Progress is shown only when reported by the runtime.",
        ),
        IpcMessage::Artifact {
            reference,
            description,
        } => (
            "ARTIFACT",
            reference.clone(),
            description.clone(),
            "The message points another agent to durable work product.",
        ),
        IpcMessage::ReviewRequest {
            subject,
            instructions,
        } => (
            "REVIEW REQUEST",
            subject.clone(),
            instructions.clone(),
            "The sender is asking another agent to review a bounded concern.",
        ),
        IpcMessage::Warning { subject, message } => (
            "WARNING",
            subject.clone(),
            message.clone(),
            "The sender is calling attention to a risk.",
        ),
        IpcMessage::Handoff { summary } => (
            "HANDOFF",
            "Work handed off".to_owned(),
            summary.clone(),
            "Responsibility is being transferred between agents.",
        ),
        IpcMessage::ContractUpdate {
            subject,
            revision,
            value,
        } => (
            "CONTRACT UPDATE",
            subject.clone(),
            format!("Revision {revision}: {value}"),
            "The project contract changed and may affect dependent work.",
        ),
        IpcMessage::OwnershipRequest { resource, reason } => (
            "OWNERSHIP REQUEST",
            resource.clone(),
            reason.clone(),
            "The sender is asking to coordinate access to a shared resource.",
        ),
    };
    MessagePresentation {
        route,
        kind: kind.to_owned(),
        title,
        body,
        why: why.to_owned(),
    }
}

pub fn tool_state_label(state: ToolState) -> &'static str {
    match state {
        ToolState::Validated => "Validated",
        ToolState::AwaitingApproval => "Awaiting approval",
        ToolState::Approved => "Approved",
        ToolState::Executing => "Running",
        ToolState::Executed => "Executed",
        ToolState::Verified => "Verified successfully",
        ToolState::Committed => "Committed",
        ToolState::Rejected => "Rejected",
        ToolState::Failed => "Failed",
        ToolState::Compensated => "Compensated",
    }
}

pub fn tool_name(name: &str) -> String {
    match name {
        "filesystem.inspect" => "Inspect files".to_owned(),
        "filesystem.write" => "Write file".to_owned(),
        "filesystem.copy" => "Copy files".to_owned(),
        "filesystem.remove" => "Remove files".to_owned(),
        other => title_words(other),
    }
}

pub fn context_lifecycle(value: orynth_context::ContextLifecycle) -> (&'static str, &'static str) {
    use orynth_context::ContextLifecycle;
    match value {
        ContextLifecycle::Active => ("Active", "Current project information available to agents."),
        ContextLifecycle::Stale => ("Outdated", "The information may need to be refreshed."),
        ContextLifecycle::Archived => (
            "Archived",
            "The information is retained but no longer current.",
        ),
        ContextLifecycle::ArchivedStale => (
            "Archived and outdated",
            "The information is retained for history.",
        ),
        ContextLifecycle::Invalidated => (
            "Invalid",
            "A dependency changed and this information is no longer trusted.",
        ),
        ContextLifecycle::Superseded => {
            ("Replaced", "A newer revision is now the current version.")
        }
    }
}

pub fn knowledge_name(namespace: &str) -> String {
    match namespace {
        "auth.schema" => "Authentication contract".to_owned(),
        "schema.users" => "Users database schema".to_owned(),
        "project.database" => "Project database definition".to_owned(),
        other => title_words(other),
    }
}

pub fn event_kind_name(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::RunCreated { .. } => "run.created",
        EventKind::TaskCreated { .. } => "task.created",
        EventKind::AgentCreated { .. } => "agent.created",
        EventKind::ModelRequested { .. } => "model.selected",
        EventKind::ModelResponseMetadata { .. } => "model.response_metadata",
        EventKind::ModelChunkReceived { .. } => "model.output",
        EventKind::ModelCompleted { .. } => "model.completed",
        EventKind::ModelFinishedWithoutUsage { .. } => "model.finished_without_usage",
        EventKind::ModelTurnCompleted { .. } => "model.turn_completed",
        EventKind::ModelTurnOutcome { .. } => "model.turn_outcome",
        EventKind::ModelTurnCancelled { .. } => "model.turn_cancelled",
        EventKind::ModelTurnFailed { .. } => "model.turn_failed",
        EventKind::ModelCancelled { .. } => "model.cancelled",
        EventKind::ModelFailed { .. } => "model.failed",
        EventKind::RunCompleted { .. } => "run.completed",
        EventKind::RunCancelled { .. } => "run.cancelled",
        EventKind::RunFailed { .. } => "run.failed",
        EventKind::ArtifactCreated { .. } => "artifact.created",
        EventKind::ContextTransition { .. } => "knowledge.changed",
        EventKind::CacheObserved { .. } => "cache.observed",
        EventKind::AgentMessage { .. } => "message.sent",
        EventKind::AssumptionTransition { .. } => "conflict.changed",
        EventKind::SchedulerTransition { .. } => "runtime.policy.changed",
        EventKind::AgentPaused { .. } => "agent.paused",
        EventKind::AgentResumed { .. } => "agent.resumed",
        EventKind::CapabilityTransition { .. } => "permission.changed",
        EventKind::ToolTransition { .. } => "tool.changed",
        EventKind::FailureMemoryTransition { .. } => "failure.memory.changed",
        EventKind::SpecialistTransition { .. } => "specialist.profile.changed",
        EventKind::ConversationTurn { .. } => "conversation.turn",
    }
}

fn context_event_presentation(
    _recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    _involved: &mut Vec<String>,
) -> (String, String, String) {
    match decode_context(version, payload) {
        Ok(ContextTransition::Created { block, .. })
        | Ok(ContextTransition::CreatedFromArtifact { block, .. }) => (
            "Knowledge became available".to_owned(),
            format!(
                "{} is now available as project information.",
                block.namespace
            ),
            "Agents can use this active knowledge according to its visibility scope.".to_owned(),
        ),
        Ok(ContextTransition::Invalidated { block, reason, .. }) => (
            "Knowledge became outdated".to_owned(),
            format!("{} is no longer current: {reason}", block),
            "Dependent work should treat this information as needing review.".to_owned(),
        ),
        Ok(ContextTransition::Superseded(reference)) => (
            "Knowledge was replaced".to_owned(),
            format!("A newer revision replaced {reference}."),
            "The older revision remains available for history.".to_owned(),
        ),
        Ok(ContextTransition::Archived(reference)) => (
            "Knowledge was archived".to_owned(),
            format!("{reference} is retained outside the active knowledge set."),
            "Archived information is not projected as current knowledge.".to_owned(),
        ),
        Ok(ContextTransition::Restored(reference)) => (
            "Knowledge was restored".to_owned(),
            format!("{reference} is current again."),
            "The runtime made this information available to projections.".to_owned(),
        ),
        Ok(ContextTransition::Pinned(reference)) => (
            "Knowledge was pinned".to_owned(),
            format!("{reference} is protected from ordinary eviction."),
            "Pinned information remains important to the context policy.".to_owned(),
        ),
        Ok(ContextTransition::Unpinned(reference)) => (
            "Knowledge was unpinned".to_owned(),
            format!("{reference} is no longer pinned."),
            "The context policy may now manage it normally.".to_owned(),
        ),
        Err(_) => (
            "Knowledge changed".to_owned(),
            "A context transition was recorded.".to_owned(),
            "Technical transition details are available in the inspector.".to_owned(),
        ),
    }
}

fn message_event_presentation(
    recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    involved: &mut Vec<String>,
) -> (String, String, String) {
    if let Ok(message) = IpcEnvelope::decode(version, payload) {
        let presentation = message_presentation(recovered, &message);
        involved.push(agent_label(recovered, message.sender));
        involved.push(agent_label(recovered, message.recipient));
        return (
            format!(
                "{} sent a {}",
                presentation.route,
                presentation.kind.to_lowercase()
            ),
            format!("{}: {}", presentation.title, first_line(&presentation.body)),
            presentation.why,
        );
    }
    (
        "Agent communication was recorded".to_owned(),
        "A structured message was added to the run history.".to_owned(),
        "The full typed message is available in technical details.".to_owned(),
    )
}

fn assumption_event_presentation(
    recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    involved: &mut Vec<String>,
) -> (String, String, String) {
    match decode_assumption(version, payload) {
        Ok(AssumptionTransition::Created { assumption }) => {
            let owner = agent_label(recovered, assumption.owner);
            involved.push(owner.clone());
            (format!("{owner} recorded an assumption"), format!("{owner} believes {} is {}.", assumption.subject, assumption.normalized_value), "This is a project claim, not an automatically verified fact.".to_owned())
        }
        Ok(AssumptionTransition::ConflictDetected { conflict }) => ("Agents disagree about a project contract".to_owned(), format!("{} has conflicting values.", conflict.subject), "The disagreement may affect dependent work and needs review.".to_owned()),
        Ok(AssumptionTransition::StateChanged { .. }) => ("An assumption status changed".to_owned(), "A project claim moved through its lifecycle.".to_owned(), "The conflict view explains whether the claim is active, conflicted, resolved, or invalid.".to_owned()),
        Err(_) => ("A project assumption changed".to_owned(), "A typed assumption transition was recorded.".to_owned(), "Technical transition details are available in the inspector.".to_owned()),
    }
}

fn scheduler_event_presentation(
    recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    involved: &mut Vec<String>,
) -> (String, String, String) {
    match decode_scheduler(version, payload) {
        Ok(orynth_scheduler::SchedulerTransition::HealthSignaled { agent_id, signal }) => {
            let name = agent_label(recovered, agent_id);
            involved.push(name.clone());
            let signal_text = health_signal_label(signal);
            (
                format!("{name} health changed"),
                format!("{name} reported {signal_text}."),
                "Health signals help the coordinator identify work that needs attention."
                    .to_owned(),
            )
        }
        Ok(orynth_scheduler::SchedulerTransition::OwnershipClaimed { agent_id, resource }) => {
            let name = agent_label(recovered, agent_id);
            involved.push(name.clone());
            (
                format!("{name} took ownership of a resource"),
                format!("{name} owns {resource}."),
                "Ownership prevents conflicting writes to shared project resources.".to_owned(),
            )
        }
        Ok(orynth_scheduler::SchedulerTransition::BudgetConfigured { agent_id, .. }) => {
            let name = agent_label(recovered, agent_id);
            involved.push(name.clone());
            (
                format!("A budget was set for {name}"),
                format!("{name} now has an explicit runtime budget."),
                "Budgets bound token, time, tool, child-agent, and context use.".to_owned(),
            )
        }
        _ => (
            "Runtime policy changed".to_owned(),
            "The scheduler updated budgets, health, ownership, or team structure.".to_owned(),
            "Technical policy transitions remain available in event details.".to_owned(),
        ),
    }
}

fn capability_event_presentation(
    recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    involved: &mut Vec<String>,
) -> (String, String, String) {
    match decode_capability(version, payload) {
        Ok(orynth_security::CapabilityTransition::Granted { lease }) => {
            let name = agent_label(recovered, lease.agent_id);
            involved.push(name.clone());
            (
                format!("{name} received permission"),
                format!(
                    "{name} can access {} in the {} domain.",
                    lease.resource,
                    format!("{:?}", lease.domain).to_lowercase()
                ),
                "The permission is scoped and must still pass checks at the effect boundary."
                    .to_owned(),
            )
        }
        Ok(orynth_security::CapabilityTransition::Revoked {
            agent_id, resource, ..
        }) => {
            let name = agent_label(recovered, agent_id);
            involved.push(name.clone());
            (
                format!("Permission was removed from {name}"),
                format!("Access to {resource} is no longer leased."),
                "Future effects must be denied unless another valid lease exists.".to_owned(),
            )
        }
        Err(_) => (
            "Permissions changed".to_owned(),
            "A capability transition was recorded.".to_owned(),
            "Technical permission details are available in the inspector.".to_owned(),
        ),
    }
}

fn tool_event_presentation(
    recovered: &RecoveredRun,
    version: u16,
    payload: &[u8],
    involved: &mut Vec<String>,
) -> (String, String, String) {
    match decode_tool(version, payload) {
        Ok(ToolTransition::Proposed {
            proposal, state, ..
        }) => {
            let name = agent_label(recovered, proposal.agent_id);
            involved.push(name.clone());
            (format!("{name} proposed a tool action"), format!("{} — {}.", tool_name(&proposal.tool_name), tool_state_label(state)), "The action is subject to permission, ownership, approval, and verification checks.".to_owned())
        }
        Ok(ToolTransition::StateChanged { state, detail, .. }) => (
            "A tool action changed state".to_owned(),
            format!(
                "Tool action is {}{}.",
                tool_state_label(state),
                detail.map_or(String::new(), |value| format!(" — {value}"))
            ),
            "The tool pipeline records each safety boundary before an effect is trusted."
                .to_owned(),
        ),
        Ok(ToolTransition::Previewed { preview, .. }) => (
            "A tool action was previewed".to_owned(),
            preview.summary,
            "The preview describes intended impact before execution.".to_owned(),
        ),
        Ok(ToolTransition::Repaired { .. }) => (
            "A tool action was repaired safely".to_owned(),
            "Only deterministic syntax/schema-safe changes were applied.".to_owned(),
            "Semantic intent was not guessed by the runtime.".to_owned(),
        ),
        Err(_) => (
            "A tool action changed".to_owned(),
            "A typed tool transition was recorded.".to_owned(),
            "Technical transaction details are available in the inspector.".to_owned(),
        ),
    }
}

fn health_signal_label(signal: HealthSignal) -> &'static str {
    match signal {
        HealthSignal::AssumptionConflict => "an assumption conflict",
        HealthSignal::AssumptionConflictResolved => "an assumption conflict was resolved",
        HealthSignal::Failure => "a failure",
        HealthSignal::FailureResolved => "a failure was resolved",
        HealthSignal::ToolError => "a tool error",
        HealthSignal::ToolErrorResolved => "a tool error was resolved",
        HealthSignal::VerificationFailure => "a verification failure",
        HealthSignal::VerificationFailureResolved => "a verification failure was resolved",
        HealthSignal::ContextPressure => "context pressure",
        HealthSignal::ContextPressureResolved => "context pressure was resolved",
        HealthSignal::BudgetPressure => "budget pressure",
        HealthSignal::BudgetPressureResolved => "budget pressure was resolved",
        HealthSignal::DependencyInvalidation => "a dependency invalidation",
        HealthSignal::DependencyInvalidationResolved => "a dependency invalidation was resolved",
        HealthSignal::NoProgress => "no progress",
        HealthSignal::NoProgressResolved => "progress resumed",
        HealthSignal::Progress => "progress",
    }
}

fn title_words(value: &str) -> String {
    value
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_line(value: &str) -> &str {
    value.lines().next().unwrap_or(value)
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none recorded".to_owned()
    } else {
        values.join(", ")
    }
}
