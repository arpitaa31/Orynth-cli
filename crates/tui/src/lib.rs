//! Projection-backed runtime inspection.
//!
//! This crate deliberately renders recovered runtime state only. It does not
//! call providers, mutate the event store, or invent sample state for an
//! operator view.

use std::{collections::BTreeMap, fmt, fmt::Write};

use orynth_assumptions::AssumptionTransition;
use orynth_context::{ContextFreshnessPolicy, ContextTransition};
use orynth_failure_memory::FailureTransition;
use orynth_kernel::{AgentId, EventKind, ModelRef};
use orynth_runtime::RecoveredRun;
use orynth_security::CapabilityTransition;
use orynth_tool_runtime::{ToolState, ToolTransition};

mod fullscreen;

pub use fullscreen::{
    RunSummary, TuiDataSource, TuiSnapshot, render_snapshot_for_terminal, run_fullscreen,
};

const MAX_RECENT_EVENTS: usize = 8;
const MAX_EVENT_DETAIL_CHARS: usize = 512;
const MAX_BREAKPOINTS: usize = 32;
const MAX_PANE_ITEMS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BreakpointKind {
    AssumptionConflict,
    ContextInvalidation,
    ApprovalRequired,
    CapabilityGranted,
    ModelChanged,
    SemanticToolRepair,
    ToolFailure,
    FailureRecorded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectorPane {
    Overview,
    Agents,
    Events,
    Breakpoints,
}

impl InspectorPane {
    pub const ALL: [Self; 4] = [
        Self::Overview,
        Self::Agents,
        Self::Events,
        Self::Breakpoints,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Agents => "agents",
            Self::Events => "events",
            Self::Breakpoints => "breakpoints",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectorAction {
    NextPane,
    PreviousPane,
    NextItem,
    PreviousItem,
    Select(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InspectorState {
    pane: InspectorPane,
    selected_index: usize,
}

impl InspectorState {
    pub const fn new() -> Self {
        Self {
            pane: InspectorPane::Overview,
            selected_index: 0,
        }
    }

    pub const fn pane(self) -> InspectorPane {
        self.pane
    }

    pub const fn selected_index(self) -> usize {
        self.selected_index
    }

    pub fn apply(&mut self, action: InspectorAction, item_count: usize) {
        match action {
            InspectorAction::NextPane => self.shift_pane(1),
            InspectorAction::PreviousPane => self.shift_pane(-1),
            InspectorAction::NextItem => {
                if item_count > 0 {
                    self.selected_index = (self.selected_index + 1).min(item_count - 1);
                }
            }
            InspectorAction::PreviousItem => {
                self.selected_index = self.selected_index.saturating_sub(1);
            }
            InspectorAction::Select(index) => {
                self.selected_index = index.min(item_count.saturating_sub(1));
            }
        }
    }

    pub fn set_pane(&mut self, pane: InspectorPane, item_count: usize) {
        self.pane = pane;
        self.selected_index = self.selected_index.min(item_count.saturating_sub(1));
    }

    fn shift_pane(&mut self, delta: isize) {
        let current = InspectorPane::ALL
            .iter()
            .position(|pane| *pane == self.pane)
            .expect("all inspector panes must be listed");
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            (current + delta as usize).min(InspectorPane::ALL.len() - 1)
        };
        if next != current {
            self.pane = InspectorPane::ALL[next];
            self.selected_index = 0;
        }
    }
}

impl Default for InspectorState {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BreakpointKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::AssumptionConflict => "assumption-conflict",
            Self::ContextInvalidation => "context-invalidation",
            Self::ApprovalRequired => "approval-required",
            Self::CapabilityGranted => "capability-granted",
            Self::ModelChanged => "model-changed",
            Self::SemanticToolRepair => "semantic-tool-repair",
            Self::ToolFailure => "tool-failure",
            Self::FailureRecorded => "failure-recorded",
        };
        formatter.write_str(label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticBreakpoint {
    pub sequence: u64,
    pub kind: BreakpointKind,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BreakpointScanError {
    Context(String),
    Assumptions(String),
    Capabilities(String),
    Tools(String),
    FailureMemory(String),
}

impl fmt::Display for BreakpointScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(error) => write!(formatter, "context event decode failed: {error}"),
            Self::Assumptions(error) => {
                write!(formatter, "assumption event decode failed: {error}")
            }
            Self::Capabilities(error) => {
                write!(formatter, "capability event decode failed: {error}")
            }
            Self::Tools(error) => write!(formatter, "tool event decode failed: {error}"),
            Self::FailureMemory(error) => {
                write!(formatter, "failure-memory event decode failed: {error}")
            }
        }
    }
}

impl std::error::Error for BreakpointScanError {}

fn bounded_detail(detail: String) -> String {
    let mut chars = detail.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_EVENT_DETAIL_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// Find semantic debugger stops by decoding the authoritative event stream.
///
/// This scanner is intentionally a pure projection operation: it cannot pause
/// or mutate a live run. Interactive pause/replay controls can build on these
/// stable event coordinates later.
pub fn scan_semantic_breakpoints(
    recovered: &RecoveredRun,
) -> Result<Vec<SemanticBreakpoint>, BreakpointScanError> {
    let mut breakpoints = Vec::new();
    let mut models = BTreeMap::<AgentId, ModelRef>::new();
    for stored in &recovered.events {
        match &stored.event.kind {
            EventKind::ModelRequested { agent_id, model } => {
                if let Some(previous) = models.insert(*agent_id, model.clone())
                    && previous != *model
                {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind: BreakpointKind::ModelChanged,
                        detail: format!(
                            "{}: {}/{} -> {}/{}",
                            agent_id,
                            previous.provider,
                            previous.model,
                            model.provider,
                            model.model
                        ),
                    });
                }
            }
            EventKind::ContextTransition { version, payload } => {
                let transition = orynth_context::decode_transition(*version, payload)
                    .map_err(|error| BreakpointScanError::Context(error.to_string()))?;
                if let ContextTransition::Invalidated { reason, .. } = transition {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind: BreakpointKind::ContextInvalidation,
                        detail: bounded_detail(reason),
                    });
                }
            }
            EventKind::AssumptionTransition { version, payload } => {
                let transition = orynth_assumptions::decode_transition(*version, payload)
                    .map_err(|error| BreakpointScanError::Assumptions(error.to_string()))?;
                if let AssumptionTransition::ConflictDetected { conflict } = transition {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind: BreakpointKind::AssumptionConflict,
                        detail: bounded_detail(conflict.subject),
                    });
                }
            }
            EventKind::CapabilityTransition { version, payload } => {
                let transition = orynth_security::decode_transition(*version, payload)
                    .map_err(|error| BreakpointScanError::Capabilities(error.to_string()))?;
                if let CapabilityTransition::Granted { lease } = transition {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind: BreakpointKind::CapabilityGranted,
                        detail: bounded_detail(format!("{:?} {}", lease.domain, lease.resource)),
                    });
                }
            }
            EventKind::ToolTransition { version, payload } => {
                let transition = orynth_tool_runtime::decode_transition(*version, payload)
                    .map_err(|error| BreakpointScanError::Tools(error.to_string()))?;
                let hit = match transition {
                    ToolTransition::Proposed {
                        state: ToolState::AwaitingApproval,
                        ..
                    } => Some((
                        BreakpointKind::ApprovalRequired,
                        "tool proposal entered approval gate".to_owned(),
                    )),
                    ToolTransition::Repaired { audit, .. } => Some((
                        BreakpointKind::SemanticToolRepair,
                        format!("repair tier {:?}", audit.tier),
                    )),
                    ToolTransition::StateChanged {
                        state: ToolState::Failed,
                        detail,
                        ..
                    } => Some((
                        BreakpointKind::ToolFailure,
                        detail.unwrap_or_else(|| "tool transaction failed".to_owned()),
                    )),
                    _ => None,
                };
                if let Some((kind, detail)) = hit {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind,
                        detail: bounded_detail(detail),
                    });
                }
            }
            EventKind::FailureMemoryTransition { version, payload } => {
                let transition = orynth_failure_memory::decode_transition(*version, payload)
                    .map_err(|error| BreakpointScanError::FailureMemory(error.to_string()))?;
                if let FailureTransition::Recorded { record } = transition {
                    breakpoints.push(SemanticBreakpoint {
                        sequence: stored.sequence,
                        kind: BreakpointKind::FailureRecorded,
                        detail: bounded_detail(format!(
                            "{}: {}",
                            record.fingerprint, record.reason
                        )),
                    });
                }
            }
            _ => {}
        }
        if breakpoints.len() == MAX_BREAKPOINTS {
            break;
        }
    }
    Ok(breakpoints)
}

/// Render a bounded text inspector for one recovered run.
pub fn render_runtime_inspector(recovered: &RecoveredRun) -> String {
    let state = &recovered.state;
    let mut output = String::new();
    writeln!(output, "Orynth runtime inspector").expect("writing to String cannot fail");
    writeln!(output, "Run: {}", recovered.run_id).expect("writing to String cannot fail");
    writeln!(output, "Status: {:?}", state.status).expect("writing to String cannot fail");
    writeln!(output, "Events: {}", recovered.events.len()).expect("writing to String cannot fail");
    writeln!(
        output,
        "Tasks: {}  Agents: {}  Artifacts: {}",
        state.tasks.len(),
        state.agents.len(),
        state.artifacts.len()
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Context: {} blocks, {} namespaces, {} subscriptions, {} invalidations",
        recovered.context.block_count(),
        recovered.context.namespace_count(),
        recovered.context.subscription_count(),
        recovered.context.invalidation_count()
    )
    .expect("writing to String cannot fail");
    let freshness = recovered
        .context
        .proprioception(ContextFreshnessPolicy::default());
    writeln!(
        output,
        "Freshness: {} active ({} tokens), {} stale, {} archived, {} invalidated, {} pinned",
        freshness.active_blocks,
        freshness.active_tokens,
        freshness.stale_blocks,
        freshness.archived_blocks,
        freshness.invalidated_blocks,
        freshness.pinned_blocks
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Messages: {}  Assumptions: {}  Conflicts: {}",
        recovered.messages.len(),
        recovered.assumptions.assumptions().len(),
        recovered.assumptions.conflicts().len()
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Tools: {}  Capabilities: {}  Cache records: {}",
        recovered.tools.records().len(),
        recovered.capabilities.leases().len(),
        recovered.cache_telemetry.len()
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Failures: {}  Active failures: {}",
        recovered.failures.records().len(),
        recovered.failures.active_records().count()
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Manager projection: {} active failures, {} cache records, {} artifacts",
        recovered.manager.active_failure_count,
        recovered.manager.cache_observation_count,
        recovered.manager.artifact_count
    )
    .expect("writing to String cannot fail");
    writeln!(
        output,
        "Specialists: {}",
        recovered.specialists.profiles().len()
    )
    .expect("writing to String cannot fail");

    match scan_semantic_breakpoints(recovered) {
        Ok(breakpoints) => {
            writeln!(output, "Semantic breakpoints: {}", breakpoints.len())
                .expect("writing to String cannot fail");
            for breakpoint in breakpoints {
                writeln!(
                    output,
                    "  #{} {} {}",
                    breakpoint.sequence, breakpoint.kind, breakpoint.detail
                )
                .expect("writing to String cannot fail");
            }
        }
        Err(error) => {
            writeln!(output, "Semantic breakpoints: unavailable ({error})")
                .expect("writing to String cannot fail");
        }
    }

    writeln!(output, "Agents:").expect("writing to String cannot fail");
    if recovered.manager.agents.is_empty() {
        writeln!(output, "  (none)").expect("writing to String cannot fail");
    } else {
        for agent in recovered.manager.agents.values() {
            let specialist = agent.specialist.as_ref().map_or_else(
                || "role=-".to_owned(),
                |profile| {
                    format!(
                        "role={} promotable={} scope={} subscriptions={} capabilities={}",
                        bounded_detail(profile.role.clone()),
                        profile.promotable,
                        profile.scope.len(),
                        profile.subscriptions.len(),
                        profile.capabilities.len()
                    )
                },
            );
            writeln!(
                output,
                "  {} {} {} model={}/{} class={:?} status={:?} health={:?} chunks={} tokens={}",
                agent.agent_id,
                agent.name,
                specialist,
                agent.model.provider,
                agent.model.model,
                agent.model.class,
                agent.status,
                agent.health.status,
                agent.chunks_received,
                agent.usage.total_tokens()
            )
            .expect("writing to String cannot fail");
        }
    }

    writeln!(output, "Recent events (max {MAX_RECENT_EVENTS}):")
        .expect("writing to String cannot fail");
    for event in recovered.events.iter().rev().take(MAX_RECENT_EVENTS).rev() {
        let detail = bounded_detail(format!("{:?}", event.event.kind));
        writeln!(output, "  #{} {detail}", event.sequence).expect("writing to String cannot fail");
    }
    output
}

pub fn inspector_pane_item_count(recovered: &RecoveredRun, pane: InspectorPane) -> usize {
    match pane {
        InspectorPane::Overview => 1,
        InspectorPane::Agents => recovered.manager.agents.len().min(MAX_PANE_ITEMS),
        InspectorPane::Events => recovered.events.len().min(MAX_PANE_ITEMS),
        InspectorPane::Breakpoints => scan_semantic_breakpoints(recovered)
            .map_or(0, |breakpoints| breakpoints.len().min(MAX_PANE_ITEMS)),
    }
}

/// Render one selectable inspector pane using a terminal-independent state.
pub fn render_inspector_pane(recovered: &RecoveredRun, state: InspectorState) -> String {
    match state.pane() {
        InspectorPane::Overview => render_runtime_inspector(recovered),
        InspectorPane::Agents => {
            let mut output = format!("Agents pane (selected {})\n", state.selected_index());
            for (index, agent) in recovered
                .manager
                .agents
                .values()
                .take(MAX_PANE_ITEMS)
                .enumerate()
            {
                let marker = if index == state.selected_index() {
                    ">"
                } else {
                    " "
                };
                writeln!(
                    output,
                    "{marker} [{index}] {} {} model={}/{} status={:?} health={:?}",
                    agent.agent_id,
                    agent.name,
                    agent.model.provider,
                    agent.model.model,
                    agent.status,
                    agent.health.status
                )
                .expect("writing to String cannot fail");
            }
            if recovered.manager.agents.is_empty() {
                output.push_str("  (none)\n");
            }
            output
        }
        InspectorPane::Events => {
            let mut output = format!("Events pane (selected {})\n", state.selected_index());
            for (index, event) in recovered.events.iter().take(MAX_PANE_ITEMS).enumerate() {
                let marker = if index == state.selected_index() {
                    ">"
                } else {
                    " "
                };
                writeln!(
                    output,
                    "{marker} [{index}] #{} {}",
                    event.sequence,
                    bounded_detail(format!("{:?}", event.event.kind))
                )
                .expect("writing to String cannot fail");
            }
            output
        }
        InspectorPane::Breakpoints => {
            let mut output = format!("Breakpoints pane (selected {})\n", state.selected_index());
            match scan_semantic_breakpoints(recovered) {
                Ok(breakpoints) if breakpoints.is_empty() => {
                    output.push_str("  (none)\n");
                }
                Ok(breakpoints) => {
                    for (index, breakpoint) in breakpoints.iter().take(MAX_PANE_ITEMS).enumerate() {
                        let marker = if index == state.selected_index() {
                            ">"
                        } else {
                            " "
                        };
                        writeln!(
                            output,
                            "{marker} [{index}] #{} {} {}",
                            breakpoint.sequence, breakpoint.kind, breakpoint.detail
                        )
                        .expect("writing to String cannot fail");
                    }
                }
                Err(error) => {
                    writeln!(output, "  unavailable: {error}")
                        .expect("writing to String cannot fail");
                }
            }
            output
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_assumptions::AssumptionGraph;
    use orynth_cache::CacheTelemetry;
    use orynth_context::{ContextGraph, ContextProprioception};
    use orynth_event_store::{InMemoryEventStore, RunStatus, RuntimeState, StoredEvent};
    use orynth_failure_memory::{
        FAILURE_MEMORY_SCHEMA_VERSION, FailureMemory, FailureRecord, FailureTransition,
        encode_transition,
    };
    use orynth_kernel::{Event, EventKind, ModelClass, ModelRef, RunId};
    use orynth_runtime::ManagerProjection;
    use orynth_scheduler::SchedulerState;
    use orynth_security::CapabilityPolicy;
    use orynth_specialist::SpecialistRegistry;
    use orynth_tool_runtime::ToolHistory;

    #[test]
    fn inspector_renders_recovered_projection_without_side_effects() {
        let run_id = RunId::from_u64(7);
        let event = Event::new(run_id, EventKind::RunCreated { run_id });
        let stored = StoredEvent { sequence: 0, event };
        let _state = InMemoryEventStore::new();
        let recovered = RecoveredRun {
            run_id,
            state: RuntimeState {
                run_id,
                status: RunStatus::Active,
                tasks: Default::default(),
                agents: Default::default(),
                artifacts: Default::default(),
                events_applied: 1,
            },
            context: ContextGraph::new(),
            events: vec![stored],
            cache_telemetry: CacheTelemetry::new(),
            messages: Vec::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
            manager: ManagerProjection {
                run_id,
                agents: Default::default(),
                context: ContextProprioception {
                    active_blocks: 0,
                    active_tokens: 0,
                    stale_blocks: 0,
                    archived_blocks: 0,
                    invalidated_blocks: 0,
                    pinned_blocks: 0,
                    largest_blocks: Vec::new(),
                    recent_invalidations: Vec::new(),
                    active_tokens_over_budget: false,
                    stale_blocks_over_budget: false,
                },
                cache_observation_count: 0,
                artifact_count: 0,
                active_failure_count: 0,
            },
        };
        let rendered = render_runtime_inspector(&recovered);
        assert!(rendered.contains("Orynth runtime inspector"));
        assert!(rendered.contains("run-0000000000000007"));
        assert!(rendered.contains("Context: 0 blocks"));
        assert!(rendered.contains("Freshness: 0 active"));
        assert!(rendered.contains("RunCreated"));
    }

    #[test]
    fn breakpoint_scan_detects_a_persisted_model_change() {
        let run_id = RunId::from_u64(8);
        let first = ModelRef {
            provider: "local".to_owned(),
            model: "cheap".to_owned(),
            class: ModelClass::Cheap,
        };
        let second = ModelRef {
            provider: "remote".to_owned(),
            model: "strong".to_owned(),
            class: ModelClass::Strong,
        };
        let agent_id = AgentId::from_u64(9);
        let events = vec![
            StoredEvent {
                sequence: 0,
                event: Event::new(run_id, EventKind::RunCreated { run_id }),
            },
            StoredEvent {
                sequence: 1,
                event: Event::new(
                    run_id,
                    EventKind::ModelRequested {
                        agent_id,
                        model: first,
                    },
                ),
            },
            StoredEvent {
                sequence: 2,
                event: Event::new(
                    run_id,
                    EventKind::ModelRequested {
                        agent_id,
                        model: second,
                    },
                ),
            },
        ];
        let recovered = RecoveredRun {
            run_id,
            state: RuntimeState {
                run_id,
                status: RunStatus::Active,
                tasks: Default::default(),
                agents: Default::default(),
                artifacts: Default::default(),
                events_applied: 3,
            },
            context: ContextGraph::new(),
            events,
            cache_telemetry: CacheTelemetry::new(),
            messages: Vec::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
            manager: ManagerProjection {
                run_id,
                agents: Default::default(),
                context: ContextProprioception {
                    active_blocks: 0,
                    active_tokens: 0,
                    stale_blocks: 0,
                    archived_blocks: 0,
                    invalidated_blocks: 0,
                    pinned_blocks: 0,
                    largest_blocks: Vec::new(),
                    recent_invalidations: Vec::new(),
                    active_tokens_over_budget: false,
                    stale_blocks_over_budget: false,
                },
                cache_observation_count: 0,
                artifact_count: 0,
                active_failure_count: 0,
            },
        };
        let breakpoints = scan_semantic_breakpoints(&recovered).expect("events decode");
        assert_eq!(breakpoints.len(), 1);
        assert_eq!(breakpoints[0].sequence, 2);
        assert_eq!(breakpoints[0].kind, BreakpointKind::ModelChanged);
    }

    #[test]
    fn breakpoint_scan_detects_a_persisted_failure_memory_record() {
        let run_id = RunId::from_u64(10);
        let record = FailureRecord::new(
            AgentId::from_u64(11),
            "auth-token-parser-v1",
            "permissive token split",
            "malformed claims were accepted",
        );
        let payload = encode_transition(&FailureTransition::Recorded { record })
            .expect("failure transition should encode");
        let recovered = RecoveredRun {
            run_id,
            state: RuntimeState {
                run_id,
                status: RunStatus::Active,
                tasks: Default::default(),
                agents: Default::default(),
                artifacts: Default::default(),
                events_applied: 2,
            },
            context: ContextGraph::new(),
            events: vec![
                StoredEvent {
                    sequence: 0,
                    event: Event::new(run_id, EventKind::RunCreated { run_id }),
                },
                StoredEvent {
                    sequence: 1,
                    event: Event::new(
                        run_id,
                        EventKind::FailureMemoryTransition {
                            version: FAILURE_MEMORY_SCHEMA_VERSION,
                            payload,
                        },
                    ),
                },
            ],
            cache_telemetry: CacheTelemetry::new(),
            messages: Vec::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
            manager: ManagerProjection {
                run_id,
                agents: Default::default(),
                context: ContextProprioception {
                    active_blocks: 0,
                    active_tokens: 0,
                    stale_blocks: 0,
                    archived_blocks: 0,
                    invalidated_blocks: 0,
                    pinned_blocks: 0,
                    largest_blocks: Vec::new(),
                    recent_invalidations: Vec::new(),
                    active_tokens_over_budget: false,
                    stale_blocks_over_budget: false,
                },
                cache_observation_count: 0,
                artifact_count: 0,
                active_failure_count: 0,
            },
        };
        let breakpoints = scan_semantic_breakpoints(&recovered).expect("events decode");
        assert_eq!(breakpoints.len(), 1);
        assert_eq!(breakpoints[0].sequence, 1);
        assert_eq!(breakpoints[0].kind, BreakpointKind::FailureRecorded);
        assert!(breakpoints[0].detail.contains("auth-token-parser-v1"));
    }

    #[test]
    fn inspector_state_supports_bounded_pane_and_item_navigation() {
        let mut state = InspectorState::new();
        state.apply(InspectorAction::NextPane, 3);
        assert_eq!(state.pane(), InspectorPane::Agents);
        state.apply(InspectorAction::NextItem, 3);
        state.apply(InspectorAction::NextItem, 3);
        assert_eq!(state.selected_index(), 2);
        state.apply(InspectorAction::NextItem, 2);
        assert_eq!(state.selected_index(), 1);
        state.apply(InspectorAction::PreviousPane, 4);
        assert_eq!(state.pane(), InspectorPane::Overview);
        assert_eq!(state.selected_index(), 0);
    }
}
