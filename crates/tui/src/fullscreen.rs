//! Full-screen, read-only runtime debugger.
//!
//! The terminal layer owns only view state, selection, filtering, and
//! rendering. A caller supplies authoritative snapshots through TuiDataSource.

use std::{
    io::{self, Write},
    time::Duration,
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use orynth_context::{ContextPrincipal, ContextTrustPolicy, ProjectionRequest};
use orynth_event_store::StoredEvent;
use orynth_kernel::{AgentId, RunId};
use orynth_runtime::RecoveredRun;
use orynth_scheduler::HealthStatus;

const MAX_EVENT_ROWS: usize = 64;
const MAX_DETAIL_CHARS: usize = 360;
const MAX_CONTEXT_ROWS: usize = 32;
const MAX_MESSAGE_ROWS: usize = 48;
const MAX_TOOL_ROWS: usize = 48;
const MAX_POLICY_ROWS: usize = 64;
const MAX_ASSUMPTION_ROWS: usize = 48;
const EVENT_PANEL_ROWS: usize = 8;
const MAX_AGENT_INDEXES: usize = 4096;
const MAX_EVENT_INDEXES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummary {
    pub run_id: RunId,
    pub status: String,
    pub event_count: usize,
}

#[derive(Clone, Debug)]
pub struct TuiSnapshot {
    pub selected: Option<RecoveredRun>,
    pub runs: Vec<RunSummary>,
}

pub trait TuiDataSource {
    fn snapshot(&mut self) -> Result<TuiSnapshot, String>;
    fn select_run(&mut self, run_id: RunId) -> Result<(), String>;

    /// Load one bounded page of older events before `before_sequence`.
    ///
    /// Sources that do not have historical paging can retain the default
    /// empty result; the view continues to show its bounded snapshot window.
    fn event_page(
        &mut self,
        _before_sequence: Option<u64>,
        _limit: usize,
    ) -> Result<Vec<StoredEvent>, String> {
        Ok(Vec::new())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Dashboard,
    Agents,
    Events,
    Context,
    Messages,
    Tools,
    Policy,
    Assumptions,
    Runs,
    Help,
}

impl Screen {
    const ALL: [Self; 9] = [
        Self::Dashboard,
        Self::Agents,
        Self::Events,
        Self::Context,
        Self::Messages,
        Self::Tools,
        Self::Policy,
        Self::Assumptions,
        Self::Runs,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Agents => "agents",
            Self::Events => "events",
            Self::Context => "context",
            Self::Messages => "messages",
            Self::Tools => "tools",
            Self::Policy => "policy",
            Self::Assumptions => "assumptions",
            Self::Runs => "runs",
            Self::Help => "help",
        }
    }

    fn from_digit(value: char) -> Option<Self> {
        match value {
            '1' => Some(Self::Dashboard),
            '2' => Some(Self::Agents),
            '3' => Some(Self::Events),
            '4' => Some(Self::Context),
            '5' => Some(Self::Messages),
            '6' => Some(Self::Tools),
            '7' => Some(Self::Policy),
            '8' => Some(Self::Assumptions),
            '9' => Some(Self::Runs),
            _ => None,
        }
    }

    fn shift(self, delta: isize) -> Self {
        if self == Self::Help {
            return Self::Dashboard;
        }
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let next = if delta.is_negative() {
            index.saturating_sub(delta.unsigned_abs())
        } else {
            (index + delta as usize).min(Self::ALL.len() - 1)
        };
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputMode {
    Filter,
}

#[derive(Clone, Debug)]
struct TuiApp {
    snapshot: TuiSnapshot,
    screen: Screen,
    selected_agent: usize,
    selected_event: usize,
    selected_run: usize,
    filter: String,
    input_mode: Option<InputMode>,
    event_detail: Option<usize>,
    event_page: Vec<StoredEvent>,
    status_message: Option<String>,
    dirty: bool,
}

impl TuiApp {
    fn new(snapshot: TuiSnapshot) -> Self {
        Self {
            snapshot,
            screen: Screen::Dashboard,
            selected_agent: 0,
            selected_event: 0,
            selected_run: 0,
            filter: String::new(),
            input_mode: None,
            event_detail: None,
            event_page: Vec::new(),
            status_message: None,
            dirty: true,
        }
    }

    fn refresh<S: TuiDataSource>(&mut self, source: &mut S) {
        match source.snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.event_page.clear();
                self.event_detail = None;
                self.clamp_selection();
                self.status_message = Some("refreshed from authoritative state".to_owned());
            }
            Err(error) => self.status_message = Some(format!("refresh failed: {error}")),
        }
        self.dirty = true;
    }

    fn clamp_selection(&mut self) {
        self.selected_agent = self
            .agent_ids()
            .len()
            .checked_sub(1)
            .map_or(0, |last| self.selected_agent.min(last));
        self.selected_event = self
            .event_indices()
            .len()
            .checked_sub(1)
            .map_or(0, |last| self.selected_event.min(last));
        self.selected_run = self
            .snapshot
            .runs
            .len()
            .checked_sub(1)
            .map_or(0, |last| self.selected_run.min(last));
    }

    fn agent_ids(&self) -> Vec<AgentId> {
        let Some(recovered) = &self.snapshot.selected else {
            return Vec::new();
        };
        let filter = self.filter.to_ascii_lowercase();
        flattened_agents(recovered)
            .into_iter()
            .filter(|(agent_id, _)| {
                filter.is_empty()
                    || recovered.manager.agents.get(agent_id).is_some_and(|agent| {
                        agent.name.to_ascii_lowercase().contains(&filter)
                            || agent.agent_id.to_string().contains(&filter)
                            || agent.mission.to_ascii_lowercase().contains(&filter)
                    })
            })
            .map(|(agent_id, _)| agent_id)
            .take(MAX_AGENT_INDEXES)
            .collect()
    }

    fn event_indices(&self) -> Vec<usize> {
        let Some(recovered) = &self.snapshot.selected else {
            return Vec::new();
        };
        let filter = self.filter.to_ascii_lowercase();
        self.event_records(recovered)
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                filter.is_empty()
                    || format!("{:?}", event.event.kind)
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .map(|(index, _)| index)
            .take(MAX_EVENT_INDEXES)
            .collect()
    }

    fn event_records<'a>(&'a self, recovered: &'a RecoveredRun) -> &'a [StoredEvent] {
        if self.event_page.is_empty() {
            &recovered.events
        } else {
            &self.event_page
        }
    }

    fn selected_agent_id(&self) -> Option<AgentId> {
        self.agent_ids().get(self.selected_agent).copied()
    }

    fn selected_event_index(&self) -> Option<usize> {
        self.event_indices().get(self.selected_event).copied()
    }

    fn handle_key<S: TuiDataSource>(
        &mut self,
        key: KeyEvent,
        source: &mut S,
    ) -> Result<bool, String> {
        if self.input_mode.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.input_mode = None;
                    self.filter.clear();
                }
                KeyCode::Enter => self.input_mode = None,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.clamp_selection();
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.filter.push(character);
                    self.clamp_selection();
                }
                _ => {}
            }
            self.dirty = true;
            return Ok(false);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            KeyCode::Char('?') => self.screen = Screen::Help,
            KeyCode::Esc => {
                self.event_detail = None;
                if self.screen == Screen::Help {
                    self.screen = Screen::Dashboard;
                }
            }
            KeyCode::Tab => self.screen = self.screen.shift(1),
            KeyCode::BackTab => self.screen = self.screen.shift(-1),
            KeyCode::Char('/') => self.input_mode = Some(InputMode::Filter),
            KeyCode::Char('r') => self.refresh(source),
            KeyCode::Char('[') => self.load_older_events(source),
            KeyCode::Char(']') => {
                self.event_page.clear();
                self.event_detail = None;
                self.status_message = Some("showing newest event window".to_owned());
            }
            KeyCode::Char('R') => self.screen = Screen::Runs,
            KeyCode::Char('d') | KeyCode::Char('D') => self.screen = Screen::Dashboard,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char(character) => {
                if let Some(screen) = Screen::from_digit(character) {
                    self.screen = screen;
                    self.event_detail = None;
                }
            }
            KeyCode::Home => self.move_to_start(),
            KeyCode::End => self.move_to_end(),
            KeyCode::Enter => self.activate_selection(source)?,
            _ => {}
        }
        self.clamp_selection();
        self.dirty = true;
        Ok(false)
    }

    fn move_selection(&mut self, delta: isize) {
        let count = match self.screen {
            Screen::Agents | Screen::Context | Screen::Policy => self.agent_ids().len(),
            Screen::Events => self.event_indices().len(),
            Screen::Runs => self.snapshot.runs.len(),
            _ => 0,
        };
        let target = match self.screen {
            Screen::Agents | Screen::Context | Screen::Policy => &mut self.selected_agent,
            Screen::Events => &mut self.selected_event,
            Screen::Runs => &mut self.selected_run,
            _ => return,
        };
        if count == 0 {
            *target = 0;
        } else if delta.is_negative() {
            *target = target.saturating_sub(delta.unsigned_abs());
        } else {
            *target = (*target + delta as usize).min(count - 1);
        }
    }

    fn move_to_start(&mut self) {
        match self.screen {
            Screen::Agents | Screen::Context | Screen::Policy => self.selected_agent = 0,
            Screen::Events => self.selected_event = 0,
            Screen::Runs => self.selected_run = 0,
            _ => {}
        }
    }

    fn move_to_end(&mut self) {
        match self.screen {
            Screen::Agents | Screen::Context | Screen::Policy => {
                self.selected_agent = self.agent_ids().len().saturating_sub(1)
            }
            Screen::Events => self.selected_event = self.event_indices().len().saturating_sub(1),
            Screen::Runs => self.selected_run = self.snapshot.runs.len().saturating_sub(1),
            _ => {}
        }
    }

    fn activate_selection<S: TuiDataSource>(&mut self, source: &mut S) -> Result<(), String> {
        match self.screen {
            Screen::Runs => {
                if let Some(summary) = self.snapshot.runs.get(self.selected_run) {
                    source.select_run(summary.run_id)?;
                    self.refresh(source);
                    self.screen = Screen::Dashboard;
                }
            }
            Screen::Events => self.event_detail = self.selected_event_index(),
            _ => {}
        }
        Ok(())
    }

    fn load_older_events<S: TuiDataSource>(&mut self, source: &mut S) {
        let Some(recovered) = &self.snapshot.selected else {
            self.status_message = Some("select a run before paging events".to_owned());
            return;
        };
        let before = self
            .event_records(recovered)
            .first()
            .map(|event| event.sequence);
        let Some(before) = before else {
            self.status_message = Some("no events available".to_owned());
            return;
        };
        match source.event_page(Some(before), MAX_EVENT_ROWS * 8) {
            Ok(events) if events.is_empty() => {
                self.status_message = Some("no older events available".to_owned());
            }
            Ok(events) => {
                self.event_page = events;
                self.selected_event = 0;
                self.event_detail = None;
                self.status_message = Some("loaded older event page".to_owned());
            }
            Err(error) => self.status_message = Some(format!("event paging failed: {error}")),
        }
    }

    fn render_lines(&self, width: u16, height: u16) -> Vec<RenderLine> {
        let width = width.max(1) as usize;
        let height = height.max(1) as usize;
        let mut lines = Vec::new();
        self.render_header(&mut lines, width);
        if width < 40 || height < 8 {
            lines.push(RenderLine::colored(
                fit(
                    "Terminal too small for the debugger; resize to at least 40x8.",
                    width,
                ),
                Color::Yellow,
                true,
            ));
        } else {
            match self.screen {
                Screen::Dashboard => self.render_dashboard(&mut lines, width, height),
                Screen::Agents => self.render_agents(&mut lines, width, height),
                Screen::Events => self.render_events(&mut lines, width, height),
                Screen::Context => self.render_context(&mut lines, width),
                Screen::Messages => self.render_messages(&mut lines, width),
                Screen::Tools => self.render_tools(&mut lines, width),
                Screen::Policy => self.render_policy(&mut lines, width),
                Screen::Assumptions => self.render_assumptions(&mut lines, width),
                Screen::Runs => self.render_runs(&mut lines, width),
                Screen::Help => self.render_help(&mut lines, width),
            }
        }
        let footer = if self.input_mode.is_some() {
            format!(" FILTER: {}_  [Enter] apply  [Esc] clear", self.filter)
        } else if let Some(message) = &self.status_message {
            format!(" {message}  |  [/] filter  [?] help  [q] quit")
        } else {
            " [Tab] next view  [Enter] inspect  [r] refresh  [?] help  [q] quit".to_owned()
        };
        lines.push(RenderLine::plain(fit(&footer, width)));
        lines.truncate(height);
        lines
    }

    fn render_header(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let (run_label, run_status, event_count, agent_count, warning_count) =
            if let Some(recovered) = &self.snapshot.selected {
                (
                    recovered.run_id.to_string(),
                    format!("{:?}", recovered.state.status).to_ascii_uppercase(),
                    recovered.state.events_applied,
                    recovered.manager.agents.len(),
                    recovered.manager.active_failure_count
                        + recovered.assumptions.conflicts().len(),
                )
            } else {
                ("no run selected".to_owned(), "EMPTY".to_owned(), 0, 0, 0)
            };
        let title = format!(
            " ORYNTH // RUNTIME CONTROL ROOM   Run: {run_label}   State: {run_status}   Events: {event_count}   Agents: {agent_count}   Warnings: {warning_count}"
        );
        lines.push(RenderLine::colored(fit(&title, width), Color::Cyan, true));
        lines.push(RenderLine::plain(fit(&format!(" View: {:<12}  [1] dash [2] agents [3] events [4] context [5] IPC [6] tools [7] policy [8] assumptions [9] runs", self.screen.label()), width)));
    }

    fn render_dashboard(&self, lines: &mut Vec<RenderLine>, width: usize, height: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        let agent_rows = height.saturating_sub(EVENT_PANEL_ROWS + 5).max(4);
        let left_width = (width * 38 / 100).max(28).min(width.saturating_sub(12));
        let ids = self.agent_ids();
        let tree = flattened_agents(recovered);
        let mut left = vec!["AGENTS".to_owned()];
        if ids.is_empty() {
            left.push("  (none)".to_owned());
        } else {
            for (index, agent_id) in ids.iter().take(agent_rows.saturating_sub(1)).enumerate() {
                if let Some(agent) = recovered.manager.agents.get(agent_id) {
                    let depth = tree
                        .iter()
                        .find(|(id, _)| id == agent_id)
                        .map_or(0, |(_, depth)| *depth);
                    left.push(format!(
                        "{}{} {:<10} {}",
                        if index == self.selected_agent {
                            ">"
                        } else {
                            " "
                        },
                        "  ".repeat(depth),
                        ellipsis(&agent.name, 12),
                        agent_status(agent.status, agent.health.status)
                    ));
                }
            }
        }
        let mut right = vec!["SELECTED AGENT".to_owned()];
        if let Some(agent) = self.selected_agent_projection(recovered) {
            right.extend(agent_detail_lines(recovered, agent));
        } else {
            right.push("  Select an agent with Up/Down or j/k".to_owned());
            right.push("  Runtime data is empty; no placeholder agent is shown.".to_owned());
        }
        for index in 0..left.len().max(right.len()).min(agent_rows) {
            let left_value = left.get(index).map_or("", String::as_str);
            let right_value = right.get(index).map_or("", String::as_str);
            lines.push(RenderLine::plain(format!(
                "{} │ {}",
                pad(fit(left_value, left_width), left_width),
                fit(right_value, width.saturating_sub(left_width + 3))
            )));
        }
        lines.push(RenderLine::colored(
            " EVENTS".to_owned(),
            Color::Yellow,
            true,
        ));
        for event in recent_event_lines(recovered, EVENT_PANEL_ROWS.saturating_sub(2)) {
            lines.push(RenderLine::plain(fit(&event, width)));
        }
    }

    fn render_agents(&self, lines: &mut Vec<RenderLine>, width: usize, height: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        let ids = self.agent_ids();
        lines.push(RenderLine::colored(
            format!(
                " AGENT TREE  ({}/{})",
                self.selected_agent.saturating_add(1),
                ids.len()
            ),
            Color::Green,
            true,
        ));
        let tree = flattened_agents(recovered);
        for (index, agent_id) in ids.iter().take(height.saturating_sub(5)).enumerate() {
            if let Some(agent) = recovered.manager.agents.get(agent_id) {
                let depth = tree
                    .iter()
                    .find(|(id, _)| id == agent_id)
                    .map_or(0, |(_, depth)| *depth);
                let marker = if index == self.selected_agent {
                    ">"
                } else {
                    " "
                };
                lines.push(RenderLine::colored(
                    fit(
                        &format!(
                            "{marker} {}{}  {:<18} model={}/{}  {}",
                            "  ".repeat(depth),
                            agent.agent_id,
                            ellipsis(&agent.name, 18),
                            ellipsis(&agent.model.provider, 10),
                            ellipsis(&agent.model.model, 16),
                            agent_status(agent.status, agent.health.status)
                        ),
                        width,
                    ),
                    status_color(agent.health.status),
                    index == self.selected_agent,
                ));
            }
        }
        lines.push(RenderLine::colored(
            " SELECTED AGENT".to_owned(),
            Color::Yellow,
            true,
        ));
        if let Some(agent) = self.selected_agent_projection(recovered) {
            for value in agent_detail_lines(recovered, agent) {
                lines.push(RenderLine::plain(fit(&value, width)));
            }
        }
    }

    fn render_events(&self, lines: &mut Vec<RenderLine>, width: usize, height: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        let indices = self.event_indices();
        lines.push(RenderLine::colored(
            format!(
                " EVENT TIMELINE  ({}/{})  {}  filter={:?}",
                self.selected_event.saturating_add(1),
                indices.len(),
                if self.event_page.is_empty() {
                    "newest window"
                } else {
                    "older page"
                },
                self.filter
            ),
            Color::Yellow,
            true,
        ));
        let visible = height.saturating_sub(6).min(MAX_EVENT_ROWS);
        let start = centered_start(self.selected_event, indices.len(), visible);
        for (row, list_index) in indices.iter().skip(start).take(visible).enumerate() {
            let event = &self.event_records(recovered)[*list_index];
            let marker = if start + row == self.selected_event {
                ">"
            } else {
                " "
            };
            lines.push(RenderLine::plain(fit(
                &format!(
                    "{marker} {:>6}  {:<28} {}",
                    event.sequence,
                    event_kind_name(&event.event.kind),
                    bounded(&format!("{:?}", event.event.kind), 140)
                ),
                width,
            )));
        }
        if let Some(index) = self.event_detail
            && let Some(event) = self.event_records(recovered).get(index)
        {
            lines.push(RenderLine::colored(
                " EVENT DETAILS".to_owned(),
                Color::Magenta,
                true,
            ));
            lines.push(RenderLine::plain(fit(
                &format!(
                    "sequence={} id={} run={} at_ms={}",
                    event.sequence, event.event.id, event.event.run_id, event.event.occurred_at_ms
                ),
                width,
            )));
            lines.push(RenderLine::plain(fit(
                &bounded(
                    &format!("payload: {:?}", event.event.kind),
                    MAX_DETAIL_CHARS,
                ),
                width,
            )));
        }
    }

    fn render_context(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        lines.push(RenderLine::colored(
            " CONTEXT INSPECTOR  (runtime visibility enforced)".to_owned(),
            Color::Blue,
            true,
        ));
        let principal = self
            .selected_agent_id()
            .map_or(ContextPrincipal::Runtime, ContextPrincipal::Agent);
        let projection = recovered.context.project(
            principal,
            &ProjectionRequest {
                namespace_patterns: vec!["*".to_owned()],
                include_stale: true,
                max_blocks: Some(MAX_CONTEXT_ROWS),
                max_tokens: Some(16_384),
                trust_policy: ContextTrustPolicy::AllowAll,
            },
        );
        lines.push(RenderLine::plain(fit(
            &format!(
                "visible_blocks={} estimated_tokens={} truncated={} principal={:?}",
                projection.blocks.len(),
                projection.estimated_tokens,
                projection.truncated,
                principal
            ),
            width,
        )));
        if projection.blocks.is_empty() {
            lines.push(RenderLine::plain("No visible context blocks.".to_owned()));
        }
        for projected in projection.blocks.iter().take(MAX_CONTEXT_ROWS) {
            let block = &projected.block;
            lines.push(RenderLine::plain(fit(&format!("{} ns={} kind={:?} rev={} scope={:?} lifecycle={:?} trust={:?} tokens={} deps={} sources={} pinned={}", block.reference(), block.namespace, block.kind, block.revision, block.scope, block.lifecycle, block.trust, block.token_estimate, block.dependencies.len(), block.sources.len(), block.pinned), width)));
        }
    }

    fn render_messages(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        lines.push(RenderLine::colored(
            " IPC / AGENT MESSAGES  (typed, not a transcript)".to_owned(),
            Color::Blue,
            true,
        ));
        if recovered.messages.is_empty() {
            lines.push(RenderLine::plain("No persisted agent messages.".to_owned()));
        }
        for message in recovered.messages.iter().rev().take(MAX_MESSAGE_ROWS) {
            lines.push(RenderLine::plain(fit(
                &format!(
                    "{} -> {}  {:?}",
                    message.sender, message.recipient, message.payload
                ),
                width,
            )));
        }
    }

    fn render_tools(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        lines.push(RenderLine::colored(
            " TOOL TRANSACTIONS".to_owned(),
            Color::Red,
            true,
        ));
        if recovered.tools.records().is_empty() {
            lines.push(RenderLine::plain(
                "No persisted tool transactions.".to_owned(),
            ));
        }
        for record in recovered.tools.records().values().rev().take(MAX_TOOL_ROWS) {
            lines.push(RenderLine::plain(fit(
                &format!(
                    "{} agent={} tool={} state={:?} detail={} preview={}",
                    record.transaction_id,
                    record.proposal.agent_id,
                    record.proposal.tool_name,
                    record.state,
                    record.detail.as_deref().unwrap_or("N/A"),
                    record
                        .preview
                        .as_ref()
                        .map_or("N/A", |preview| preview.summary.as_str())
                ),
                width,
            )));
        }
    }

    fn render_policy(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        lines.push(RenderLine::colored(
            " PERMISSIONS / OWNERSHIP / BUDGET".to_owned(),
            Color::Red,
            true,
        ));
        if let Some(agent_id) = self.selected_agent_id() {
            lines.push(RenderLine::plain(format!("Selected agent: {agent_id}")));
            if let Some(budget) = recovered.scheduler.budget(agent_id) {
                lines.push(RenderLine::plain(fit(
                    &format!(
                        "budget used: {:?} limits: {:?}",
                        budget.usage, budget.limits
                    ),
                    width,
                )));
            } else {
                lines.push(RenderLine::plain("budget: N/A".to_owned()));
            }
            for lease in recovered
                .capabilities
                .leases()
                .values()
                .filter(|lease| lease.agent_id == agent_id)
                .take(MAX_POLICY_ROWS)
            {
                lines.push(RenderLine::plain(fit(
                    &format!(
                        "capability {:?} {} expires={}",
                        lease.domain, lease.resource, lease.expires_at_ms
                    ),
                    width,
                )));
            }
            for (resource, _) in recovered
                .scheduler
                .ownership()
                .iter()
                .filter(|(_, owner)| **owner == agent_id)
                .take(MAX_POLICY_ROWS)
            {
                lines.push(RenderLine::plain(fit(
                    &format!("write ownership {resource}"),
                    width,
                )));
            }
        } else {
            lines.push(RenderLine::plain("No agent selected.".to_owned()));
        }
        lines.push(RenderLine::plain(format!(
            "cache observations: {}",
            recovered.cache_telemetry.len()
        )));
    }

    fn render_assumptions(&self, lines: &mut Vec<RenderLine>, width: usize) {
        let Some(recovered) = &self.snapshot.selected else {
            self.render_empty(lines, width);
            return;
        };
        lines.push(RenderLine::colored(
            " ASSUMPTIONS / CONFLICTS".to_owned(),
            Color::Magenta,
            true,
        ));
        if recovered.assumptions.assumptions().is_empty() {
            lines.push(RenderLine::plain("No assumptions recorded.".to_owned()));
        }
        for assumption in recovered
            .assumptions
            .assumptions()
            .values()
            .take(MAX_ASSUMPTION_ROWS)
        {
            lines.push(RenderLine::plain(fit(
                &format!(
                    "{} owner={} subject={} value={} state={:?} trust={:?}",
                    assumption.id,
                    assumption.owner,
                    assumption.subject,
                    assumption.normalized_value,
                    assumption.state,
                    assumption.trust
                ),
                width,
            )));
        }
        for conflict in recovered
            .assumptions
            .conflicts()
            .values()
            .take(MAX_ASSUMPTION_ROWS)
        {
            lines.push(RenderLine::colored(
                fit(
                    &format!(
                        "CONFLICT {} {} <-> {} owners={:?}",
                        conflict.subject, conflict.left, conflict.right, conflict.affected_agents
                    ),
                    width,
                ),
                Color::Red,
                true,
            ));
        }
    }

    fn render_runs(&self, lines: &mut Vec<RenderLine>, width: usize) {
        lines.push(RenderLine::colored(
            " RUNS  (select a persisted authoritative run)".to_owned(),
            Color::Cyan,
            true,
        ));
        if self.snapshot.runs.is_empty() {
            lines.push(RenderLine::plain("No Orynth runs found.".to_owned()));
            lines.push(RenderLine::plain(
                "Run orynth tui --demo for an offline deterministic runtime demo.".to_owned(),
            ));
            return;
        }
        for (index, run) in self.snapshot.runs.iter().enumerate() {
            let marker = if index == self.selected_run { ">" } else { " " };
            lines.push(RenderLine::plain(fit(
                &format!(
                    "{marker} {}  {:<10} events={}",
                    run.run_id, run.status, run.event_count
                ),
                width,
            )));
        }
    }

    fn render_help(&self, lines: &mut Vec<RenderLine>, width: usize) {
        lines.push(RenderLine::colored(
            " HELP / KEYBOARD".to_owned(),
            Color::Cyan,
            true,
        ));
        for line in [
            "Up/Down or j/k  move selection        Enter  inspect event / select run",
            "Tab / Shift-Tab  change view           1..9  jump to a view",
            "/  filter current agents/events        r  reload authoritative source",
            "R  run list                             Esc  close detail/help",
            "q or Ctrl-C  quit and restore terminal",
            "",
            "All displayed values come from the supplied runtime snapshot.",
            "Controls are read-only in this phase; no direct event append is exposed.",
        ] {
            lines.push(RenderLine::plain(fit(line, width)));
        }
    }

    fn render_empty(&self, lines: &mut Vec<RenderLine>, width: usize) {
        lines.push(RenderLine::colored(
            " EMPTY RUNTIME".to_owned(),
            Color::Yellow,
            true,
        ));
        lines.push(RenderLine::plain(fit(
            "No selected Orynth run is available.",
            width,
        )));
        lines.push(RenderLine::plain(fit(
            "Press 9 for persisted runs, r to refresh, or run orynth tui --demo.",
            width,
        )));
    }

    fn selected_agent_projection<'a>(
        &'a self,
        recovered: &'a RecoveredRun,
    ) -> Option<&'a orynth_runtime::ManagerAgentProjection> {
        self.selected_agent_id()
            .and_then(|agent_id| recovered.manager.agents.get(&agent_id))
    }
}

#[derive(Clone, Debug)]
struct RenderLine {
    text: String,
    color: Option<Color>,
    bold: bool,
}

impl RenderLine {
    fn plain(text: String) -> Self {
        Self {
            text,
            color: None,
            bold: false,
        }
    }

    fn colored(text: String, color: Color, bold: bool) -> Self {
        Self {
            text,
            color: Some(color),
            bold,
        }
    }
}

struct TerminalGuard {
    restored: bool,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All)) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self { restored: false })
    }

    fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        disable_raw_mode()?;
        execute!(io::stdout(), Show, LeaveAlternateScreen)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn run_fullscreen<S: TuiDataSource>(mut source: S) -> Result<(), String> {
    let snapshot = source.snapshot()?;
    let mut app = TuiApp::new(snapshot);
    let mut terminal =
        TerminalGuard::enter().map_err(|error| format!("could not enter terminal UI: {error}"))?;
    let result = loop {
        if app.dirty {
            draw(&app).map_err(|error| format!("could not render terminal UI: {error}"))?;
            app.dirty = false;
        }
        if event::poll(Duration::from_millis(250))
            .map_err(|error| format!("terminal input failed: {error}"))?
            && let Event::Key(key) =
                event::read().map_err(|error| format!("terminal input failed: {error}"))?
            && app.handle_key(key, &mut source)?
        {
            break Ok(());
        }
    };
    terminal
        .restore()
        .map_err(|error| format!("could not restore terminal: {error}"))?;
    result
}

pub fn render_snapshot_for_terminal(snapshot: &TuiSnapshot, width: u16, height: u16) -> String {
    TuiApp::new(snapshot.clone())
        .render_lines(width, height)
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw(app: &TuiApp) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let lines = app.render_lines(width, height);
    let mut stdout = io::stdout();
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    for (row, line) in lines.iter().enumerate() {
        queue!(stdout, MoveTo(0, row as u16))?;
        if let Some(color) = line.color {
            queue!(stdout, SetForegroundColor(color))?;
        }
        if line.bold {
            queue!(stdout, SetAttribute(Attribute::Bold))?;
        }
        queue!(
            stdout,
            Print(&line.text),
            ResetColor,
            SetAttribute(Attribute::Reset)
        )?;
    }
    stdout.flush()
}

fn flattened_agents(recovered: &RecoveredRun) -> Vec<(AgentId, usize)> {
    let mut children = std::collections::BTreeMap::<Option<AgentId>, Vec<AgentId>>::new();
    for (agent_id, agent) in &recovered.manager.agents {
        children.entry(agent.parent_id).or_default().push(*agent_id);
    }
    for values in children.values_mut() {
        values.sort();
    }
    let mut output = Vec::with_capacity(recovered.manager.agents.len());
    let mut stack = children
        .get(&None)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|agent_id| (agent_id, 0_usize))
        .collect::<Vec<_>>();
    while let Some((agent_id, depth)) = stack.pop() {
        output.push((agent_id, depth));
        if let Some(child_ids) = children.get(&Some(agent_id)) {
            for child_id in child_ids.iter().rev() {
                stack.push((*child_id, depth.saturating_add(1)));
            }
        }
    }
    output
}

fn agent_detail_lines(
    recovered: &RecoveredRun,
    agent: &orynth_runtime::ManagerAgentProjection,
) -> Vec<String> {
    let budget = agent.budget.map_or_else(
        || "N/A".to_owned(),
        |budget| format!("used={:?} limits={:?}", budget.usage, budget.limits),
    );
    vec![
        format!("  {}  {}", agent.agent_id, agent.name),
        format!("  Mission: {}", bounded(&agent.mission, 120)),
        format!(
            "  Model: {}/{} ({:?})",
            agent.model.provider, agent.model.model, agent.model.class
        ),
        format!(
            "  Status: {:?}   Health: {:?}",
            agent.status, agent.health.status
        ),
        format!(
            "  Parent: {}   Children: {}",
            agent
                .parent_id
                .map_or("N/A".to_owned(), |id| id.to_string()),
            agent.child_ids.len()
        ),
        format!(
            "  Progress/chunks: {}   Usage tokens: {}",
            agent.chunks_received,
            agent.usage.total_tokens()
        ),
        format!("  Budget: {budget}"),
        format!(
            "  Context: active={} tokens={} stale={} pressure={}",
            recovered.manager.context.active_blocks,
            recovered.manager.context.active_tokens,
            recovered.manager.context.stale_blocks,
            recovered.manager.context.active_tokens_over_budget
        ),
        format!(
            "  Owned resources: {}   Capabilities: {}",
            agent.owned_resources.len(),
            recovered
                .capabilities
                .leases()
                .values()
                .filter(|lease| lease.agent_id == agent.agent_id)
                .count()
        ),
        format!(
            "  Assumptions: {}   Conflicts: {}   Failures: {}",
            agent.assumption_ids.len(),
            agent.conflict_ids.len(),
            agent.active_failure_ids.len()
        ),
        format!(
            "  Specialist: {}",
            agent
                .specialist
                .as_ref()
                .map_or("N/A".to_owned(), |profile| profile.role.clone())
        ),
    ]
}

fn recent_event_lines(recovered: &RecoveredRun, limit: usize) -> Vec<String> {
    recovered
        .events
        .iter()
        .rev()
        .take(limit)
        .rev()
        .map(|event| {
            format!(
                "  {:>6}  {}",
                event.sequence,
                event_kind_name(&event.event.kind)
            )
        })
        .collect()
}

fn event_kind_name(kind: &orynth_kernel::EventKind) -> &'static str {
    match kind {
        orynth_kernel::EventKind::RunCreated { .. } => "run.created",
        orynth_kernel::EventKind::TaskCreated { .. } => "task.created",
        orynth_kernel::EventKind::AgentCreated { .. } => "agent.created",
        orynth_kernel::EventKind::ModelRequested { .. } => "model.requested",
        orynth_kernel::EventKind::ModelChunkReceived { .. } => "model.chunk",
        orynth_kernel::EventKind::ModelCompleted { .. } => "model.completed",
        orynth_kernel::EventKind::ModelCancelled { .. } => "model.cancelled",
        orynth_kernel::EventKind::ModelFailed { .. } => "model.failed",
        orynth_kernel::EventKind::RunCompleted { .. } => "run.completed",
        orynth_kernel::EventKind::RunCancelled { .. } => "run.cancelled",
        orynth_kernel::EventKind::RunFailed { .. } => "run.failed",
        orynth_kernel::EventKind::ArtifactCreated { .. } => "artifact.created",
        orynth_kernel::EventKind::ContextTransition { .. } => "context.transition",
        orynth_kernel::EventKind::CacheObserved { .. } => "cache.observed",
        orynth_kernel::EventKind::AgentMessage { .. } => "agent.message",
        orynth_kernel::EventKind::AssumptionTransition { .. } => "assumption.transition",
        orynth_kernel::EventKind::SchedulerTransition { .. } => "scheduler.transition",
        orynth_kernel::EventKind::AgentPaused { .. } => "agent.paused",
        orynth_kernel::EventKind::AgentResumed { .. } => "agent.resumed",
        orynth_kernel::EventKind::CapabilityTransition { .. } => "capability.transition",
        orynth_kernel::EventKind::ToolTransition { .. } => "tool.transition",
        orynth_kernel::EventKind::FailureMemoryTransition { .. } => "failure.transition",
        orynth_kernel::EventKind::SpecialistTransition { .. } => "specialist.transition",
    }
}

fn agent_status(status: orynth_event_store::AgentStatus, health: HealthStatus) -> String {
    let status = match status {
        orynth_event_store::AgentStatus::Created => "CREATED",
        orynth_event_store::AgentStatus::Running => "RUNNING",
        orynth_event_store::AgentStatus::Paused => "PAUSED",
        orynth_event_store::AgentStatus::Completed => "DONE",
        orynth_event_store::AgentStatus::Cancelled => "CANCELLED",
        orynth_event_store::AgentStatus::Failed => "FAILED",
    };
    let health = match health {
        HealthStatus::Healthy => "OK",
        HealthStatus::Degraded => "DEGRADED",
        HealthStatus::Blocked => "BLOCKED",
    };
    format!("{status}/{health}")
}

fn status_color(health: HealthStatus) -> Color {
    match health {
        HealthStatus::Healthy => Color::Green,
        HealthStatus::Degraded => Color::Yellow,
        HealthStatus::Blocked => Color::Red,
    }
}

fn centered_start(selected: usize, count: usize, visible: usize) -> usize {
    if count <= visible || visible == 0 {
        0
    } else {
        selected.saturating_sub(visible / 2).min(count - visible)
    }
}

fn bounded(value: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let mut chars = value.chars();
    let result = chars
        .by_ref()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}

fn ellipsis(value: &str, width: usize) -> String {
    bounded(value, width)
}

fn fit(value: &str, width: usize) -> String {
    bounded(value, width)
}

fn pad(value: String, width: usize) -> String {
    let length = value.chars().count();
    if length >= width {
        value
    } else {
        format!("{value}{}", " ".repeat(width - length))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_assumptions::AssumptionGraph;
    use orynth_cache::CacheTelemetry;
    use orynth_context::{ContextGraph, ContextProprioception};
    use orynth_event_store::{RunStatus, RuntimeState, StoredEvent};
    use orynth_failure_memory::FailureMemory;
    use orynth_kernel::{Event, EventKind};
    use orynth_scheduler::SchedulerState;
    use orynth_security::CapabilityPolicy;
    use orynth_specialist::SpecialistRegistry;
    use orynth_tool_runtime::ToolHistory;

    fn empty_snapshot() -> TuiSnapshot {
        let run_id = RunId::from_u64(700);
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
            events: vec![StoredEvent {
                sequence: 0,
                event: Event::new(run_id, EventKind::RunCreated { run_id }),
            }],
            cache_telemetry: CacheTelemetry::new(),
            messages: Vec::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures: FailureMemory::default(),
            specialists: SpecialistRegistry::default(),
            manager: orynth_runtime::ManagerProjection {
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
        TuiSnapshot {
            selected: Some(recovered),
            runs: vec![RunSummary {
                run_id,
                status: "ACTIVE".to_owned(),
                event_count: 1,
            }],
        }
    }

    #[test]
    fn frame_rendering_handles_empty_runtime_and_small_terminal() {
        let text = render_snapshot_for_terminal(
            &TuiSnapshot {
                selected: None,
                runs: Vec::new(),
            },
            12,
            6,
        );
        assert!(text.contains("Terminal"));
        assert!(!text.contains("panic"));
    }

    #[test]
    fn frame_rendering_is_bounded_and_has_real_run_identity() {
        let snapshot = empty_snapshot();
        let text = render_snapshot_for_terminal(&snapshot, 100, 30);
        assert!(text.contains("run-00000000000002bc"));
        assert!(text.contains("no placeholder agent"));
        assert!(text.lines().count() <= 30);
    }

    #[test]
    fn navigation_does_not_escape_empty_collections() {
        let mut app = TuiApp::new(TuiSnapshot {
            selected: None,
            runs: Vec::new(),
        });
        app.move_selection(1);
        app.move_to_end();
        app.move_selection(-1);
        assert_eq!(app.selected_agent, 0);
        assert_eq!(app.selected_event, 0);
        assert_eq!(app.selected_run, 0);
    }

    #[test]
    fn unicode_bounding_does_not_split_utf8() {
        assert_eq!(bounded("é漢字", 2), "é…");
    }
}
