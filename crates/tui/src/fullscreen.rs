//! Full-screen, read-only runtime control room.
//!
//! The UI owns only navigation and presentation state. Runtime truth comes
//! from `TuiDataSource` and the recovered projection it supplies.

use std::{io, time::Duration};

use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};
use orynth_context::{ContextPrincipal, ContextTrustPolicy, ProjectionRequest};
use orynth_event_store::{RunStatus, StoredEvent};
use orynth_kernel::{AgentId, RunId};
use orynth_runtime::RecoveredRun;
use orynth_scheduler::{BudgetState, HealthStatus};
use ratatui::{
    Frame, Terminal,
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table,
        TableState, Tabs, Wrap,
    },
};

use crate::presentation::{
    EventPresentation, EventSeverity, agent_label, agent_role, agent_status, context_lifecycle,
    health_marker, human_event, knowledge_name, message_presentation, model_class, model_name,
    tool_name, tool_state_label,
};
use crate::theme::THEME;

const MAX_EVENT_ROWS: usize = 64;
const MAX_DETAIL_CHARS: usize = 720;
const MAX_CONTEXT_ROWS: usize = 32;
const MAX_MESSAGE_ROWS: usize = 48;
const MAX_TOOL_ROWS: usize = 48;
const MAX_POLICY_ROWS: usize = 64;
const MAX_ASSUMPTION_ROWS: usize = 48;
const MAX_AGENT_INDEXES: usize = 4096;
const MAX_EVENT_INDEXES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummary {
    pub run_id: RunId,
    /// Optional operator-facing identity. The runtime always uses `run_id`.
    pub display_name: Option<String>,
    pub status: String,
    pub event_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TuiPresentation {
    pub title: String,
    pub description: String,
    pub demo: bool,
}

#[derive(Clone, Debug)]
pub struct TuiSnapshot {
    pub selected: Option<RecoveredRun>,
    pub runs: Vec<RunSummary>,
    pub presentation: Option<TuiPresentation>,
}

pub trait TuiDataSource {
    fn snapshot(&mut self) -> Result<TuiSnapshot, String>;
    fn select_run(&mut self, run_id: RunId) -> Result<(), String>;

    fn submit_text(&mut self, _text: String) -> Result<(), String> {
        Err("No live Coordinator is connected".into())
    }

    fn live_text(&self) -> Option<String> {
        None
    }

    fn is_live(&self) -> bool {
        false
    }

    /// Load one bounded page of older events before `before_sequence`.
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
    Overview,
    Agents,
    Activity,
    Knowledge,
    Messages,
    Tools,
    Permissions,
    Conflicts,
    Runs,
}

impl Screen {
    const ALL: [Self; 9] = [
        Self::Overview,
        Self::Agents,
        Self::Activity,
        Self::Knowledge,
        Self::Messages,
        Self::Tools,
        Self::Permissions,
        Self::Conflicts,
        Self::Runs,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Agents => "Team",
            Self::Activity => "Activity",
            Self::Knowledge => "Knowledge",
            Self::Messages => "Messages",
            Self::Tools => "Tools",
            Self::Permissions => "Access",
            Self::Conflicts => "Conflicts",
            Self::Runs => "Runs",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Overview => "What is happening right now",
            Self::Agents => "People, roles, models, and health",
            Self::Activity => "Human-readable runtime history",
            Self::Knowledge => "Project information available to agents",
            Self::Messages => "Structured communication between agents",
            Self::Tools => "Tool actions and safety checks",
            Self::Permissions => "Permissions, capabilities, and ownership",
            Self::Conflicts => "Assumptions, decisions, and disagreements",
            Self::Runs => "Persisted runtime sessions",
        }
    }

    fn from_digit(value: char) -> Option<Self> {
        Self::ALL
            .get(value.to_digit(10)?.saturating_sub(1) as usize)
            .copied()
    }

    fn shift(self, delta: isize) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let length = Self::ALL.len();
        let next = if delta.is_negative() {
            (index + length - (delta.unsigned_abs() % length)) % length
        } else {
            (index + delta as usize) % length
        };
        Self::ALL[next]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DetailTarget {
    Agent(usize),
    Event(usize),
    Message(usize),
    Knowledge(usize),
    Tool(usize),
    Permission(usize),
    Conflict(usize),
    Assumption(usize),
    Run(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    Welcome,
    Help,
    Detail(DetailTarget),
}

#[derive(Clone, Debug)]
pub(super) struct TuiApp {
    snapshot: TuiSnapshot,
    screen: Screen,
    selected_agent: usize,
    selected_event: usize,
    selected_message: usize,
    selected_knowledge: usize,
    selected_tool: usize,
    selected_permission: usize,
    selected_conflict: usize,
    show_assumptions: bool,
    selected_run: usize,
    filter: String,
    input_mode: bool,
    overlay: Option<Overlay>,
    detail_scroll: u16,
    event_page: Vec<StoredEvent>,
    status_message: Option<String>,
}

impl TuiApp {
    pub(super) fn new(snapshot: TuiSnapshot) -> Self {
        let welcome = snapshot
            .presentation
            .as_ref()
            .is_some_and(|value| value.demo);
        Self {
            snapshot,
            screen: Screen::Overview,
            selected_agent: 0,
            selected_event: 0,
            selected_message: 0,
            selected_knowledge: 0,
            selected_tool: 0,
            selected_permission: 0,
            selected_conflict: 0,
            show_assumptions: false,
            selected_run: 0,
            filter: String::new(),
            input_mode: false,
            overlay: welcome.then_some(Overlay::Welcome),
            detail_scroll: 0,
            event_page: Vec::new(),
            status_message: None,
        }
    }

    pub(super) fn embedded(snapshot: TuiSnapshot) -> Self {
        let mut app = Self::new(snapshot);
        app.overlay = None;
        app
    }

    pub(super) fn open_runs(&mut self) {
        self.screen = Screen::Runs;
        self.overlay = None;
    }

    fn recovered(&self) -> Option<&RecoveredRun> {
        self.snapshot.selected.as_ref()
    }

    fn refresh<S: TuiDataSource>(&mut self, source: &mut S) {
        match source.snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.event_page.clear();
                self.overlay = None;
                self.detail_scroll = 0;
                self.clamp_selection();
                self.status_message =
                    Some("Refreshed from authoritative runtime state.".to_owned());
            }
            Err(error) => self.status_message = Some(format!("Refresh failed: {error}")),
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_agent = self
            .selected_agent
            .min(self.agent_ids().len().saturating_sub(1));
        self.selected_event = self
            .selected_event
            .min(self.event_indices().len().saturating_sub(1));
        self.selected_message = self
            .selected_message
            .min(self.message_indices().len().saturating_sub(1));
        self.selected_knowledge = self
            .selected_knowledge
            .min(self.knowledge_indices().len().saturating_sub(1));
        self.selected_tool = self
            .selected_tool
            .min(self.tool_indices().len().saturating_sub(1));
        self.selected_permission = self
            .selected_permission
            .min(self.agent_ids().len().saturating_sub(1));
        self.selected_conflict = self
            .selected_conflict
            .min(self.conflict_indices().len().saturating_sub(1));
        self.selected_run = self
            .selected_run
            .min(self.snapshot.runs.len().saturating_sub(1));
    }

    fn agent_ids(&self) -> Vec<AgentId> {
        let Some(recovered) = self.recovered() else {
            return Vec::new();
        };
        let filter = self.filter.to_ascii_lowercase();
        recovered
            .manager
            .agents
            .iter()
            .filter_map(|(id, agent)| {
                let role = agent_role(agent);
                (filter.is_empty()
                    || agent.name.to_ascii_lowercase().contains(&filter)
                    || role.to_ascii_lowercase().contains(&filter)
                    || agent.mission.to_ascii_lowercase().contains(&filter))
                .then_some(*id)
            })
            .take(MAX_AGENT_INDEXES)
            .collect()
    }

    fn event_records(&self) -> &[StoredEvent] {
        if self.event_page.is_empty() {
            self.recovered().map_or(&[], |run| run.events.as_slice())
        } else {
            &self.event_page
        }
    }

    fn event_indices(&self) -> Vec<usize> {
        let filter = self.filter.to_ascii_lowercase();
        self.event_records()
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                filter.is_empty() || {
                    let presentation = self.recovered().map(|run| human_event(run, event));
                    presentation.is_some_and(|value| {
                        value.title.to_ascii_lowercase().contains(&filter)
                            || value.summary.to_ascii_lowercase().contains(&filter)
                    })
                }
            })
            .map(|(index, _)| index)
            .take(MAX_EVENT_INDEXES)
            .collect()
    }

    fn message_indices(&self) -> Vec<usize> {
        let Some(recovered) = self.recovered() else {
            return Vec::new();
        };
        let filter = self.filter.to_ascii_lowercase();
        recovered
            .messages
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                let view = message_presentation(recovered, message);
                filter.is_empty()
                    || view.title.to_ascii_lowercase().contains(&filter)
                    || view.body.to_ascii_lowercase().contains(&filter)
                    || view.route.to_ascii_lowercase().contains(&filter)
            })
            .map(|(index, _)| index)
            .take(MAX_MESSAGE_ROWS)
            .collect()
    }

    fn knowledge_indices(&self) -> Vec<usize> {
        let Some(recovered) = self.recovered() else {
            return Vec::new();
        };
        let principal = self
            .selected_agent_id()
            .map_or(ContextPrincipal::Runtime, ContextPrincipal::Agent);
        recovered
            .context
            .project(
                principal,
                &ProjectionRequest {
                    namespace_patterns: vec!["*".to_owned()],
                    max_blocks: Some(MAX_CONTEXT_ROWS),
                    max_tokens: Some(16_384),
                    include_stale: true,
                    trust_policy: ContextTrustPolicy::AllowAll,
                },
            )
            .blocks
            .iter()
            .enumerate()
            .map(|(index, _)| index)
            .collect()
    }

    fn tool_indices(&self) -> Vec<usize> {
        self.recovered().map_or_else(Vec::new, |run| {
            (0..run.tools.records().len().min(MAX_TOOL_ROWS)).collect()
        })
    }

    fn conflict_indices(&self) -> Vec<usize> {
        self.recovered().map_or_else(Vec::new, |run| {
            (0..run.assumptions.conflicts().len().min(MAX_ASSUMPTION_ROWS)).collect()
        })
    }

    fn assumption_indices(&self) -> Vec<usize> {
        self.recovered().map_or_else(Vec::new, |run| {
            (0..run.assumptions.assumptions().len().min(MAX_ASSUMPTION_ROWS)).collect()
        })
    }

    fn selected_agent_id(&self) -> Option<AgentId> {
        self.agent_ids().get(self.selected_agent).copied()
    }

    fn move_selection(&mut self, delta: isize) {
        let count = match self.screen {
            Screen::Agents | Screen::Permissions | Screen::Overview => self.agent_ids().len(),
            Screen::Knowledge => self.knowledge_indices().len(),
            Screen::Activity => self.event_indices().len(),
            Screen::Messages => self.message_indices().len(),
            Screen::Tools => self.tool_indices().len(),
            Screen::Conflicts => {
                if self.show_assumptions {
                    self.assumption_indices().len()
                } else {
                    self.conflict_indices().len()
                }
            }
            Screen::Runs => self.snapshot.runs.len(),
        };
        let target = match self.screen {
            Screen::Agents | Screen::Overview => &mut self.selected_agent,
            Screen::Permissions => &mut self.selected_permission,
            Screen::Knowledge => &mut self.selected_knowledge,
            Screen::Activity => &mut self.selected_event,
            Screen::Messages => &mut self.selected_message,
            Screen::Tools => &mut self.selected_tool,
            Screen::Conflicts => &mut self.selected_conflict,
            Screen::Runs => &mut self.selected_run,
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
        self.set_selection(0);
    }

    fn move_to_end(&mut self) {
        let count = match self.screen {
            Screen::Agents | Screen::Permissions | Screen::Overview => self.agent_ids().len(),
            Screen::Knowledge => self.knowledge_indices().len(),
            Screen::Activity => self.event_indices().len(),
            Screen::Messages => self.message_indices().len(),
            Screen::Tools => self.tool_indices().len(),
            Screen::Conflicts => {
                if self.show_assumptions {
                    self.assumption_indices().len()
                } else {
                    self.conflict_indices().len()
                }
            }
            Screen::Runs => self.snapshot.runs.len(),
        };
        self.set_selection(count.saturating_sub(1));
    }

    fn set_selection(&mut self, index: usize) {
        match self.screen {
            Screen::Agents | Screen::Overview => self.selected_agent = index,
            Screen::Permissions => self.selected_permission = index,
            Screen::Knowledge => self.selected_knowledge = index,
            Screen::Activity => self.selected_event = index,
            Screen::Messages => self.selected_message = index,
            Screen::Tools => self.selected_tool = index,
            Screen::Conflicts => self.selected_conflict = index,
            Screen::Runs => self.selected_run = index,
        }
        self.clamp_selection();
    }

    fn open_detail(&mut self) {
        let target = match self.screen {
            Screen::Overview => self
                .selected_agent_id()
                .map(|_| DetailTarget::Agent(self.selected_agent)),
            Screen::Agents => self
                .selected_agent_id()
                .map(|_| DetailTarget::Agent(self.selected_agent)),
            Screen::Activity => self
                .event_indices()
                .get(self.selected_event)
                .copied()
                .map(DetailTarget::Event),
            Screen::Knowledge => self
                .knowledge_indices()
                .get(self.selected_knowledge)
                .copied()
                .map(DetailTarget::Knowledge),
            Screen::Messages => self
                .message_indices()
                .get(self.selected_message)
                .copied()
                .map(DetailTarget::Message),
            Screen::Tools => self
                .tool_indices()
                .get(self.selected_tool)
                .copied()
                .map(DetailTarget::Tool),
            Screen::Permissions => self
                .selected_agent_id()
                .map(|_| DetailTarget::Permission(self.selected_permission)),
            Screen::Conflicts => {
                if self.show_assumptions {
                    self.assumption_indices()
                        .get(self.selected_conflict)
                        .copied()
                        .map(DetailTarget::Assumption)
                } else {
                    self.conflict_indices()
                        .get(self.selected_conflict)
                        .copied()
                        .map(DetailTarget::Conflict)
                }
            }
            Screen::Runs => self
                .snapshot
                .runs
                .get(self.selected_run)
                .map(|_| DetailTarget::Run(self.selected_run)),
        };
        if let Some(target) = target {
            self.detail_scroll = 0;
            self.overlay = Some(Overlay::Detail(target));
        }
    }

    fn select_run<S: TuiDataSource>(&mut self, source: &mut S) {
        if self.screen != Screen::Runs {
            return;
        }
        if let Some(run) = self.snapshot.runs.get(self.selected_run) {
            match source.select_run(run.run_id) {
                Ok(()) => self.refresh(source),
                Err(error) => self.status_message = Some(format!("Run selection failed: {error}")),
            }
        }
    }

    fn load_older_events<S: TuiDataSource>(&mut self, source: &mut S) {
        let Some(first) = self.event_records().first().map(|event| event.sequence) else {
            self.status_message = Some("There are no events to page.".to_owned());
            return;
        };
        match source.event_page(Some(first), MAX_EVENT_ROWS * 8) {
            Ok(events) if events.is_empty() => {
                self.status_message = Some("You are at the oldest available activity.".to_owned())
            }
            Ok(events) => {
                self.event_page = events;
                self.selected_event = 0;
                self.status_message = Some("Showing an older activity page.".to_owned());
            }
            Err(error) => self.status_message = Some(format!("Activity paging failed: {error}")),
        }
    }

    pub(super) fn handle_key<S: TuiDataSource>(
        &mut self,
        key: KeyEvent,
        source: &mut S,
    ) -> Result<bool, String> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.input_mode {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.input_mode = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.clamp_selection();
                }
                KeyCode::Char(value)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.filter.push(value);
                    self.clamp_selection();
                }
                _ => {}
            }
            return Ok(false);
        }
        if let Some(overlay) = self.overlay {
            match overlay {
                Overlay::Welcome => match key.code {
                    KeyCode::Enter | KeyCode::Esc => self.overlay = None,
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
                    _ => {}
                },
                Overlay::Help => match key.code {
                    KeyCode::Esc | KeyCode::Char('?') => self.overlay = None,
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
                    _ => {}
                },
                Overlay::Detail(_) => match key.code {
                    KeyCode::Esc | KeyCode::Enter => self.overlay = None,
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.detail_scroll = self.detail_scroll.saturating_sub(1)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.detail_scroll = self.detail_scroll.saturating_add(1)
                    }
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
                    _ => {}
                },
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('/') => self.input_mode = true,
            KeyCode::Char('r') => self.refresh(source),
            KeyCode::Char('[') if self.screen == Screen::Activity => self.load_older_events(source),
            KeyCode::Char(']') if self.screen == Screen::Activity => {
                self.event_page.clear();
                self.status_message = Some("Showing the newest activity page.".to_owned());
            }
            KeyCode::Char('s') if self.screen == Screen::Runs => self.select_run(source),
            KeyCode::Char('a') if self.screen == Screen::Conflicts => {
                self.show_assumptions = !self.show_assumptions;
                self.selected_conflict = 0;
            }
            KeyCode::Tab => self.screen = self.screen.shift(1),
            KeyCode::BackTab => self.screen = self.screen.shift(-1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char(value) => {
                if let Some(screen) = Screen::from_digit(value) {
                    self.screen = screen;
                }
            }
            KeyCode::Home => self.move_to_start(),
            KeyCode::End => self.move_to_end(),
            KeyCode::Enter => self.open_detail(),
            _ => {}
        }
        Ok(false)
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < 42 || area.height < 9 {
            let paragraph = Paragraph::new(vec![
                Line::styled(
                    "Terminal too small",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw("Resize to at least 42x9 to open the control room."),
                Line::raw("Press q to quit."),
            ])
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" Orynth "));
            frame.render_widget(paragraph, area);
            return;
        }
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(area);
        self.render_header(frame, vertical[0]);
        match self.screen {
            Screen::Overview => self.render_overview(frame, vertical[1]),
            Screen::Agents => self.render_agents(frame, vertical[1]),
            Screen::Activity => self.render_activity(frame, vertical[1]),
            Screen::Knowledge => self.render_knowledge(frame, vertical[1]),
            Screen::Messages => self.render_messages(frame, vertical[1]),
            Screen::Tools => self.render_tools(frame, vertical[1]),
            Screen::Permissions => self.render_permissions(frame, vertical[1]),
            Screen::Conflicts => self.render_conflicts(frame, vertical[1]),
            Screen::Runs => self.render_runs(frame, vertical[1]),
        }
        self.render_footer(frame, vertical[2]);
        if let Some(overlay) = self.overlay {
            self.render_overlay(frame, area, overlay);
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = self
            .snapshot
            .presentation
            .as_ref()
            .map(|value| value.title.clone())
            .or_else(|| {
                self.recovered().and_then(|run| {
                    run.state
                        .tasks
                        .values()
                        .next()
                        .map(|task| task.title.clone())
                })
            })
            .unwrap_or_else(|| "No run selected".to_owned());
        let run = self.recovered();
        let state = run.map_or("NO RUN".to_owned(), |value| {
            run_status_label(value.state.status).to_uppercase()
        });
        let events = run.map_or(0, |value| value.state.events_applied);
        let agents = run.map_or(0, |value| value.manager.agents.len());
        let issues = run.map_or(0, |value| {
            value.assumptions.conflicts().len() + value.manager.active_failure_count
        });
        let top = Line::from(vec![
            Span::styled(
                " ORYNTH ",
                Style::default()
                    .fg(THEME.brand)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "AI Agent Runtime / Runtime Control Room",
                Style::default().fg(THEME.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("   ·  {state}"),
                Style::default().fg(if issues == 0 {
                    THEME.success
                } else {
                    THEME.warning
                }),
            ),
        ]);
        let info = Line::from(vec![
            Span::styled(
                format!(" {title}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "   {agents} agents   {events} events   {issues} issue{}",
                if issues == 1 { "" } else { "s" }
            )),
        ]);
        let tab_labels = if area.width >= 100 {
            Screen::ALL
                .iter()
                .map(|screen| Line::from(screen.label()))
                .collect::<Vec<_>>()
        } else {
            vec![Line::from(format!(
                "{}  Â·  1-9 / Tab to change screen",
                self.screen.label()
            ))]
        };
        let tabs = Tabs::new(tab_labels)
            .select(
                Screen::ALL
                    .iter()
                    .position(|screen| *screen == self.screen)
                    .unwrap_or(0),
            )
            .highlight_style(
                Style::default()
                    .fg(THEME.brand)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().fg(THEME.muted))
            .divider("  ")
            .block(Block::default());
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(Paragraph::new(top), chunks[0]);
        frame.render_widget(
            Paragraph::new(info).style(Style::default().fg(Color::Gray)),
            chunks[1],
        );
        frame.render_widget(Paragraph::new(""), chunks[2]);
        frame.render_widget(tabs, chunks[3]);
        frame.render_widget(Paragraph::new(""), chunks[4]);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        if let Some(overlay) = self.overlay {
            let controls = match overlay {
                Overlay::Welcome => "Enter Continue   Esc Close   q Quit",
                Overlay::Help => "Esc Close   q Quit",
                Overlay::Detail(_) => "â†‘â†“ Scroll   Enter/Esc Close   q Quit",
            };
            frame.render_widget(
                Paragraph::new(Line::raw(controls))
                    .style(Style::default().fg(Color::DarkGray))
                    .block(Block::default().borders(Borders::TOP)),
                area,
            );
            return;
        }
        let has_items = self.current_item_count() > 0;
        let mut controls = if has_items {
            "↑↓ Navigate   Enter Open   Tab Next   / Filter   ? Help   q Quit".to_owned()
        } else {
            "Tab Next   r Refresh   ? Help   q Quit".to_owned()
        };
        if self.screen == Screen::Activity && has_items {
            controls.push_str("   [/] Older/Newest");
        }
        if self.screen == Screen::Runs && has_items {
            controls.push_str("   s Select");
        }
        if self.screen == Screen::Conflicts {
            controls.push_str(if self.show_assumptions {
                "   a Conflicts"
            } else {
                "   a Assumptions"
            });
        }
        if self.input_mode {
            controls = format!("Filter: {}_   Enter Apply   Esc Clear", self.filter);
        }
        if let Some(message) = &self.status_message {
            controls.push_str("   |   ");
            controls.push_str(message);
        }
        frame.render_widget(
            Paragraph::new(Line::raw(controls))
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::TOP)),
            area,
        );
    }

    fn current_item_count(&self) -> usize {
        match self.screen {
            Screen::Overview => self.agent_ids().len(),
            Screen::Agents | Screen::Permissions => self.agent_ids().len(),
            Screen::Activity => self.event_indices().len(),
            Screen::Knowledge => self.knowledge_indices().len(),
            Screen::Messages => self.message_indices().len(),
            Screen::Tools => self.tool_indices().len(),
            Screen::Conflicts => {
                if self.show_assumptions {
                    self.assumption_indices().len()
                } else {
                    self.conflict_indices().len()
                }
            }
            Screen::Runs => self.snapshot.runs.len(),
        }
    }

    fn render_overview(&self, frame: &mut Frame<'_>, area: Rect) {
        // Keep the showcase screen readable as one story: team, situation,
        // and the short timeline that explains how we got here.
        if area.width >= 100 {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
                .split(area);
            let top = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
                .split(rows[0]);
            self.render_agent_list(frame, top[0], "AI TEAM");
            self.render_attention(frame, top[1]);
            self.render_recent_activity(frame, rows[1]);
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(42),
                    Constraint::Percentage(27),
                    Constraint::Percentage(31),
                ])
                .split(area);
            self.render_agent_list(frame, rows[0], "AI TEAM");
            self.render_attention(frame, rows[1]);
            self.render_recent_activity(frame, rows[2]);
        }
    }

    fn render_agents(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                .split(area)
        };
        self.render_agent_list(frame, columns[0], "TEAM  •  ↑↓ select");
        self.render_selected_agent(frame, columns[1], "TEAM MEMBER  •  Enter opens inspector");
    }

    fn render_agent_list(&self, frame: &mut Frame<'_>, area: Rect, title: &str) {
        let Some(recovered) = self.recovered() else {
            self.render_empty(
                frame,
                area,
                "No runtime selected",
                "Press 9 for persisted runs or use the demo.",
            );
            return;
        };
        let ids = self.agent_ids();
        let compact = title == "AI TEAM";
        let items = ids
            .iter()
            .filter_map(|id| recovered.manager.agents.get(id))
            .map(|agent| {
                let (marker, _health) = health_marker(agent.health.status);
                let (lifecycle, health_text) = agent_status(agent.status, agent.health.status);
                let status_text = if lifecycle == health_text {
                    lifecycle.to_owned()
                } else {
                    format!("{lifecycle} · {health_text}")
                };
                if compact {
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{marker} "), health_style(agent.health.status)),
                        Span::styled(
                            agent.name.clone(),
                            Style::default().fg(THEME.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(format!(
                            "  ·  {}  ·  {}",
                            agent_role(agent),
                            model_name(&agent.model)
                        )),
                        Span::styled(
                            format!("  ·  {status_text}"),
                            health_style(agent.health.status),
                        ),
                    ]))
                } else {
                    ListItem::new(vec![
                        Line::from(vec![
                            Span::styled(format!("{marker} "), health_style(agent.health.status)),
                            Span::styled(
                                agent.name.clone(),
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::raw(format!("   {status_text}")),
                        ]),
                        Line::from(vec![
                            Span::raw("    "),
                            Span::styled(agent_role(agent), Style::default().fg(Color::Gray)),
                            Span::raw(" · "),
                            Span::styled(
                                format!(
                                    "{} · {}",
                                    model_name(&agent.model),
                                    model_class(&agent.model.class)
                                ),
                                Style::default().fg(THEME.accent),
                            ),
                        ]),
                    ])
                }
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select((!items.is_empty()).then_some(self.selected_agent));
        let list = List::new(items)
            .block(panel(title, Some(Screen::Agents.subtitle())))
            .highlight_style(
                Style::default()
                    .bg(THEME.surface_selected)
                    .fg(THEME.text)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn render_selected_agent(&self, frame: &mut Frame<'_>, area: Rect, title: &str) {
        let Some(recovered) = self.recovered() else {
            self.render_empty(
                frame,
                area,
                "No selected agent",
                "Runtime data will appear here when a run is selected.",
            );
            return;
        };
        let Some(agent_id) = self.selected_agent_id() else {
            self.render_empty(
                frame,
                area,
                "No selected agent",
                "Use the agent list to choose a team member.",
            );
            return;
        };
        let Some(agent) = recovered.manager.agents.get(&agent_id) else {
            return;
        };
        let (lifecycle, _health) = agent_status(agent.status, agent.health.status);
        let (marker, health_label) = health_marker(agent.health.status);
        let budget = budget_summary(agent.budget);
        let context = recovered.context.project(
            ContextPrincipal::Agent(agent_id),
            &ProjectionRequest {
                namespace_patterns: vec!["*".to_owned()],
                include_stale: true,
                max_blocks: Some(MAX_CONTEXT_ROWS),
                max_tokens: Some(16_384),
                trust_policy: ContextTrustPolicy::AllowAll,
            },
        );
        let mut lines = vec![
            Line::from(vec![Span::styled(
                format!("{marker} {}", agent.name),
                health_style(agent.health.status).add_modifier(Modifier::BOLD),
            )]),
            Line::raw(agent_role(agent)),
            Line::raw(format!(
                "{}  ·  {}  ·  {}",
                model_name(&agent.model),
                model_class(&agent.model.class),
                agent.model.provider
            )),
            Line::raw(format!("{lifecycle}  ·  {health_label}")),
            Line::raw(format!("Mission: {}", agent.mission)),
            Line::raw(format!("Budget: {budget}")),
            Line::raw(format!(
                "Knowledge: {} visible block{} / {} tokens",
                context.blocks.len(),
                plural(context.blocks.len()),
                context.estimated_tokens
            )),
            Line::raw(format!(
                "Owns: {} resource{}  ·  {} conflict{}",
                agent.owned_resources.len(),
                plural(agent.owned_resources.len()),
                agent.conflict_ids.len(),
                plural(agent.conflict_ids.len())
            )),
        ];
        if let Some(profile) = &agent.specialist {
            lines.push(Line::raw(format!(
                "Scope: {}",
                join_or_none(&profile.scope)
            )));
        }
        lines.push(Line::styled(
            "Enter opens the human summary and technical details.",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                title,
                Some("Responsibilities, model, health, and budget"),
            )),
            area,
        );
    }

    fn render_attention(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "ATTENTION NEEDED", "No run is selected.");
            return;
        };
        let conflicts = recovered
            .assumptions
            .conflicts()
            .values()
            .take(4)
            .collect::<Vec<_>>();
        let mut lines = Vec::new();
        if conflicts.is_empty() && recovered.manager.active_failure_count == 0 {
            lines.push(Line::styled(
                "✓ No active warnings",
                Style::default().fg(Color::Green),
            ));
            lines.push(Line::raw(
                "The recovered team has no unresolved attention items.",
            ));
        }
        for conflict in conflicts {
            let left = recovered.assumptions.assumptions().get(&conflict.left);
            let right = recovered.assumptions.assumptions().get(&conflict.right);
            lines.push(Line::styled(
                "! Project disagreement",
                Style::default()
                    .fg(THEME.warning)
                    .add_modifier(Modifier::BOLD),
            ));
            if let (Some(left), Some(right)) = (left, right) {
                lines.extend([
                    Line::raw(format!(
                        "  {} and {} disagree about {}",
                        agent_label(recovered, left.owner),
                        agent_label(recovered, right.owner),
                        conflict.subject
                    )),
                    Line::raw(format!(
                        "  {} believes {}",
                        agent_label(recovered, left.owner),
                        left.normalized_value
                    )),
                    Line::raw(format!(
                        "  {} believes {}",
                        agent_label(recovered, right.owner),
                        right.normalized_value
                    )),
                    Line::styled(
                        "  Orynth detected different values for the same contract.",
                        Style::default().fg(THEME.muted),
                    ),
                    Line::styled("  Enter opens Conflicts", Style::default().fg(THEME.accent)),
                ]);
            }
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                "ATTENTION NEEDED",
                Some("Warnings are actionable; press 8 for Conflicts"),
            )),
            area,
        );
    }

    fn render_recent_activity(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "RECENT ACTIVITY", "No events are available.");
            return;
        };
        let items = self
            .event_records()
            .iter()
            .rev()
            .take(8)
            .map(|event| {
                let view = human_event(recovered, event);
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("#{:<3} ", view.sequence),
                        Style::default().fg(THEME.muted),
                    ),
                    Span::styled(
                        view.title,
                        event_severity_style(&view.severity).add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items)
                .block(panel(
                    "RECENT ACTIVITY",
                    Some("Plain-English runtime history"),
                ))
                .highlight_style(Style::default().bg(Color::Rgb(30, 45, 58))),
            area,
        );
    }

    fn render_activity(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No activity", "Select a run first.");
            return;
        };
        let indices = self.event_indices();
        let rows = indices
            .iter()
            .take(MAX_EVENT_ROWS)
            .map(|index| {
                let event = &self.event_records()[*index];
                let view = human_event(recovered, event);
                Row::new(vec![
                    Cell::from(Line::styled(
                        format!("#{}", view.sequence),
                        Style::default().fg(THEME.muted),
                    )),
                    Cell::from(Line::styled(
                        view.title,
                        event_severity_style(&view.severity).add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(view.summary),
                ])
                .height(2)
            })
            .collect::<Vec<_>>();
        let mut state = TableState::default();
        state.select(
            (!rows.is_empty()).then_some(self.selected_event.min(rows.len().saturating_sub(1))),
        );
        let table = Table::new(
            rows,
            [
                Constraint::Length(7),
                Constraint::Length(28),
                Constraint::Min(20),
            ],
        )
        .header(
            Row::new(vec!["Event", "What happened", "Why it matters"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .block(panel(
            "ACTIVITY",
            Some("Enter opens the explanation and technical event record"),
        ))
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(30, 45, 58))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
        frame.render_stateful_widget(table, columns[0], &mut state);
        if let Some(index) = indices.get(self.selected_event) {
            self.render_event_preview(
                frame,
                columns[1],
                &human_event(recovered, &self.event_records()[*index]),
            );
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No activity selected",
                "There are no events matching the current filter.",
            );
        }
    }

    fn render_event_preview(&self, frame: &mut Frame<'_>, area: Rect, view: &EventPresentation) {
        let lines = vec![
            Line::styled(
                view.title.clone(),
                event_severity_style(&view.severity).add_modifier(Modifier::BOLD),
            ),
            Line::raw(view.summary.clone()),
            Line::raw(""),
            Line::styled(
                "Why it matters",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(view.why.clone()),
            Line::raw(""),
            Line::styled(
                format!("Involved: {}", join_or_none(&view.involved)),
                Style::default().fg(Color::Gray),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                "ACTIVITY INSPECTOR",
                Some("Human explanation first; technical kind remains available"),
            )),
            area,
        );
    }

    fn render_knowledge(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No knowledge", "Select a run first.");
            return;
        };
        let principal = self
            .selected_agent_id()
            .map_or(ContextPrincipal::Runtime, ContextPrincipal::Agent);
        let projection = recovered.context.project(
            principal,
            &ProjectionRequest {
                namespace_patterns: vec!["*".to_owned()],
                max_blocks: Some(MAX_CONTEXT_ROWS),
                max_tokens: Some(16_384),
                include_stale: true,
                trust_policy: ContextTrustPolicy::AllowAll,
            },
        );
        let rows = projection
            .blocks
            .iter()
            .map(|item| {
                let (label, _) = context_lifecycle(item.block.lifecycle);
                Row::new(vec![
                    Cell::from(knowledge_name(&item.block.namespace)),
                    Cell::from(label),
                    Cell::from(format!("{} tokens", item.block.token_estimate)),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = TableState::default();
        state.select(
            (!rows.is_empty()).then_some(self.selected_knowledge.min(rows.len().saturating_sub(1))),
        );
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Percentage(50),
                    Constraint::Length(18),
                    Constraint::Length(14),
                ],
            )
            .header(
                Row::new(vec!["Project information", "Lifecycle", "Size"]).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(panel(
                "KNOWLEDGE",
                Some("Context blocks, dependencies, and shared project information"),
            ))
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
            .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        if let Some(item) = projection.blocks.get(self.selected_knowledge) {
            let (label, explanation) = context_lifecycle(item.block.lifecycle);
            let lines = vec![
                Line::styled(
                    knowledge_name(&item.block.namespace),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(format!("{label} — {explanation}")),
                Line::raw(format!(
                    "Revision {}  ·  {} tokens  ·  {}",
                    item.block.revision,
                    item.block.token_estimate,
                    scope_label(item.block.scope)
                )),
                Line::raw(format!("Dependencies: {}", item.block.dependencies.len())),
                Line::raw(format!(
                    "Shared with: {} visible agent{}",
                    recovered.manager.agents.len().saturating_sub(1),
                    plural(recovered.manager.agents.len().saturating_sub(1))
                )),
                Line::raw(""),
                Line::styled(
                    "Enter opens technical context details.",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                    "KNOWLEDGE DETAILS",
                    Some("Plain lifecycle language; hashes and scopes are in details"),
                )),
                columns[1],
            );
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No knowledge selected",
                "No visible project information is available.",
            );
        }
    }

    fn render_messages(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No messages", "Select a run first.");
            return;
        };
        let indices = self.message_indices();
        let items = indices
            .iter()
            .map(|index| {
                let view = message_presentation(recovered, &recovered.messages[*index]);
                ListItem::new(vec![
                    Line::styled(
                        view.route,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(vec![
                        Span::styled(view.kind, Style::default().fg(Color::Yellow)),
                        Span::raw(format!("  {}", view.title)),
                    ]),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(
            (!items.is_empty()).then_some(self.selected_message.min(items.len().saturating_sub(1))),
        );
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(
                    "MESSAGES",
                    Some("Structured agent-to-agent communication (IPC)"),
                ))
                .highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
                .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        if let Some(index) = indices.get(self.selected_message) {
            let view = message_presentation(recovered, &recovered.messages[*index]);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        view.route,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        view.kind,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        view.title,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(""),
                    Line::raw(view.body),
                    Line::raw(""),
                    Line::styled(view.why, Style::default().fg(Color::Gray)),
                    Line::raw(""),
                    Line::styled(
                        "Enter opens the raw typed message and provenance.",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
                .wrap(Wrap { trim: true })
                .block(panel("MESSAGE", Some("Human summary first"))),
                columns[1],
            );
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No message selected",
                "No messages match the current filter.",
            );
        }
    }

    fn render_tools(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No tools", "Select a run first.");
            return;
        };
        let records = recovered
            .tools
            .records()
            .values()
            .take(MAX_TOOL_ROWS)
            .collect::<Vec<_>>();
        let rows = records
            .iter()
            .map(|record| {
                Row::new(vec![
                    Cell::from(agent_label(recovered, record.proposal.agent_id)),
                    Cell::from(tool_name(&record.proposal.tool_name)),
                    Cell::from(tool_state_label(record.state)),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = TableState::default();
        state.select(
            (!rows.is_empty()).then_some(self.selected_tool.min(rows.len().saturating_sub(1))),
        );
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(14),
                    Constraint::Percentage(42),
                    Constraint::Length(22),
                ],
            )
            .header(
                Row::new(vec!["Agent", "Action", "Result"]).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(panel(
                "TOOLS",
                Some("Tool actions, permission checks, and verification"),
            ))
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
            .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        if let Some(record) = records.get(self.selected_tool) {
            let path = record
                .proposal
                .input
                .values()
                .next()
                .cloned()
                .unwrap_or_else(|| "No resource listed".to_owned());
            let lines = vec![
                Line::styled(
                    tool_name(&record.proposal.tool_name),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(agent_label(recovered, record.proposal.agent_id)),
                Line::raw(path),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("Result: ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        tool_state_label(record.state),
                        tool_state_style(record.state),
                    ),
                ]),
                Line::raw(format!(
                    "Permission: {}",
                    if recovered
                        .capabilities
                        .leases()
                        .keys()
                        .any(|lease| lease.0 == record.proposal.agent_id)
                    {
                        "Allowed by a scoped lease"
                    } else {
                        "No matching lease shown"
                    }
                )),
                Line::raw(format!(
                    "Ownership: {}",
                    if recovered
                        .scheduler
                        .ownership()
                        .values()
                        .any(|owner| *owner == record.proposal.agent_id)
                    {
                        "Resource ownership recorded"
                    } else {
                        "No ownership recorded"
                    }
                )),
                Line::raw("Undo: Available only when the recorded effect is reversible."),
                Line::raw(""),
                Line::styled(
                    "Enter opens transaction, policy, and verification details.",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: true })
                    .block(panel("TOOL ACTION", Some("Readable transaction summary"))),
                columns[1],
            );
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No tool selected",
                "No tool transactions are recorded.",
            );
        }
    }

    fn render_permissions(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No access records", "Select a run first.");
            return;
        };
        let ids = self.agent_ids();
        let rows = ids
            .iter()
            .filter_map(|id| recovered.manager.agents.get(id))
            .map(|agent| {
                Row::new(vec![
                    Cell::from(agent.name.clone()),
                    Cell::from(format!(
                        "{} permission{}",
                        recovered
                            .capabilities
                            .leases()
                            .keys()
                            .filter(|lease| lease.0 == agent.agent_id)
                            .count(),
                        plural(
                            recovered
                                .capabilities
                                .leases()
                                .keys()
                                .filter(|lease| lease.0 == agent.agent_id)
                                .count()
                        )
                    )),
                    Cell::from(format!("{} owned", agent.owned_resources.len())),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = TableState::default();
        state.select(
            (!rows.is_empty())
                .then_some(self.selected_permission.min(rows.len().saturating_sub(1))),
        );
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(18),
                    Constraint::Length(18),
                    Constraint::Length(18),
                ],
            )
            .header(
                Row::new(vec!["Agent", "Access", "Ownership"]).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(panel(
                "ACCESS",
                Some("Permissions, capabilities, and resource ownership"),
            ))
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
            .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        if let Some(agent_id) = ids.get(self.selected_permission) {
            if let Some(agent) = recovered.manager.agents.get(agent_id) {
                let leases = recovered
                    .capabilities
                    .leases()
                    .values()
                    .filter(|lease| lease.agent_id == *agent_id)
                    .take(MAX_POLICY_ROWS)
                    .collect::<Vec<_>>();
                let mut lines = vec![
                    Line::styled(
                        agent.name.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(agent_role(agent)),
                    Line::raw(""),
                    Line::styled(
                        "Can access",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if leases.is_empty() {
                    lines.push(Line::raw("No explicit capability leases recorded."));
                } else {
                    for lease in leases {
                        lines.push(Line::raw(format!(
                            "• {}  ({})",
                            lease.resource,
                            domain_label(lease.domain)
                        )));
                    }
                }
                lines.push(Line::styled(
                    "Owns",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                if agent.owned_resources.is_empty() {
                    lines.push(Line::raw("No owned resources recorded."));
                } else {
                    lines.extend(
                        agent
                            .owned_resources
                            .iter()
                            .take(12)
                            .map(|resource| Line::raw(format!("• {resource}"))),
                    );
                }
                lines.push(Line::raw(
                    "Network access is blocked unless a scoped lease says otherwise.",
                ));
                lines.push(Line::styled(
                    "Enter opens the technical lease and ownership details.",
                    Style::default().fg(Color::DarkGray),
                ));
                frame.render_widget(
                    Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                        "ACCESS EXPLAINED",
                        Some("Capabilities are scoped authority, not general trust"),
                    )),
                    columns[1],
                );
            }
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No permission selected",
                "No agents are available.",
            );
        }
    }

    fn render_conflicts(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.show_assumptions {
            self.render_assumptions(frame, area);
            return;
        }
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No conflicts", "Select a run first.");
            return;
        };
        let conflicts = recovered
            .assumptions
            .conflicts()
            .values()
            .take(MAX_ASSUMPTION_ROWS)
            .collect::<Vec<_>>();
        if conflicts.is_empty() {
            self.render_empty(
                frame,
                area,
                "No unresolved conflicts",
                "Agents currently agree on the recovered project claims.",
            );
            return;
        }
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(33), Constraint::Percentage(67)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(area)
        };
        let items = conflicts
            .iter()
            .map(|conflict| {
                let left = recovered.assumptions.assumptions().get(&conflict.left);
                let right = recovered.assumptions.assumptions().get(&conflict.right);
                ListItem::new(vec![
                    Line::styled(
                        conflict.subject.clone(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!(
                        "{} vs {}",
                        left.map_or_else(
                            || "Agent".to_owned(),
                            |value| agent_label(recovered, value.owner)
                        ),
                        right.map_or_else(
                            || "Agent".to_owned(),
                            |value| agent_label(recovered, value.owner)
                        )
                    )),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(
            self.selected_conflict.min(items.len().saturating_sub(1)),
        ));
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(
                    "CONFLICTS",
                    Some("Deterministic disagreements found in the project"),
                ))
                .highlight_style(Style::default().bg(Color::Rgb(55, 45, 25)).fg(Color::White))
                .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        let conflict = conflicts[self
            .selected_conflict
            .min(conflicts.len().saturating_sub(1))];
        let left = recovered.assumptions.assumptions().get(&conflict.left);
        let right = recovered.assumptions.assumptions().get(&conflict.right);
        let mut lines = vec![
            Line::styled(
                "CONFLICT",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                format!("Agents disagree about: {}", conflict.subject),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
        ];
        if let Some(left) = left {
            lines.extend([
                Line::styled(
                    agent_label(recovered, left.owner),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(agent_role(
                    recovered
                        .manager
                        .agents
                        .get(&left.owner)
                        .expect("assumption owner is projected"),
                )),
                Line::raw(format!("believes: {}", left.normalized_value)),
            ]);
        }
        lines.push(Line::styled(
            "VS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        if let Some(right) = right {
            lines.extend([
                Line::styled(
                    agent_label(recovered, right.owner),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(agent_role(
                    recovered
                        .manager
                        .agents
                        .get(&right.owner)
                        .expect("assumption owner is projected"),
                )),
                Line::raw(format!("believes: {}", right.normalized_value)),
            ]);
        }
        lines.extend([
            Line::raw(""),
            Line::raw("Status: Unresolved"),
            Line::raw(format!(
                "Affected: {}",
                conflict
                    .affected_agents
                    .iter()
                    .map(|id| agent_label(recovered, *id))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Line::raw(""),
            Line::styled(
                "These agents are working with different values for the same project contract.",
                Style::default().fg(Color::Gray),
            ),
            Line::styled(
                "Enter opens evidence, revision, trust, and raw conflict details.",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                "WHY THIS MATTERS",
                Some("No generated explanation — this is derived from recorded claims"),
            )),
            columns[1],
        );
    }

    fn render_assumptions(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = if area.width >= 90 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        };
        let Some(recovered) = self.recovered() else {
            self.render_empty(frame, area, "No assumptions", "Select a run first.");
            return;
        };
        let assumptions = recovered
            .assumptions
            .assumptions()
            .values()
            .take(MAX_ASSUMPTION_ROWS)
            .collect::<Vec<_>>();
        let items = assumptions
            .iter()
            .map(|assumption| {
                let state = assumption_state_label(assumption.state);
                ListItem::new(vec![
                    Line::styled(
                        format!("{} assumes", agent_label(recovered, assumption.owner)),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!(
                        "{} is {}  ·  {state}",
                        assumption.subject, assumption.normalized_value
                    )),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(
            (!items.is_empty())
                .then_some(self.selected_conflict.min(items.len().saturating_sub(1))),
        );
        frame.render_stateful_widget(
            List::new(items)
                .block(panel(
                    "ASSUMPTIONS",
                    Some("Current project claims; press a to return to Conflicts"),
                ))
                .highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
                .highlight_symbol("› "),
            columns[0],
            &mut state,
        );
        if let Some(assumption) = assumptions.get(self.selected_conflict) {
            let lines = vec![
                Line::styled(
                    format!("{} assumes:", agent_label(recovered, assumption.owner)),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    format!("{} is {}", assumption.subject, assumption.normalized_value),
                    Style::default().fg(Color::Cyan),
                ),
                Line::raw(assumption.claim.clone()),
                Line::raw(format!(
                    "Status: {}",
                    assumption_state_label(assumption.state)
                )),
                Line::raw(format!(
                    "Confidence: {}%",
                    assumption.confidence_millis / 10
                )),
                Line::raw(format!("Evidence: {}", join_or_none(&assumption.evidence))),
                Line::raw(""),
                Line::styled(
                    "Enter opens revision, trust, and raw assumption details.",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                    "ASSUMPTION DETAILS",
                    Some("A claim is not automatically verified truth"),
                )),
                columns[1],
            );
        } else {
            self.render_empty(
                frame,
                columns[1],
                "No assumption selected",
                "No project claims are recorded.",
            );
        }
    }

    fn render_runs(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = self
            .snapshot
            .runs
            .iter()
            .enumerate()
            .map(|(index, run)| {
                Row::new(vec![
                    Cell::from(
                        run.display_name
                            .clone()
                            .unwrap_or_else(|| format!("Run {}", index + 1)),
                    ),
                    Cell::from(run_summary_status_label(&run.status)),
                    Cell::from(format!(
                        "{} agents",
                        self.snapshot
                            .selected
                            .as_ref()
                            .filter(|value| value.run_id == run.run_id)
                            .map_or(0, |value| value.manager.agents.len())
                    )),
                    Cell::from(format!("{} events", run.event_count)),
                ])
            })
            .collect::<Vec<_>>();
        let mut state = TableState::default();
        state.select(
            (!rows.is_empty()).then_some(self.selected_run.min(rows.len().saturating_sub(1))),
        );
        let block = panel(
            "RUNS",
            Some("Persisted sessions — Enter opens details, s selects the run"),
        );
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Percentage(42),
                    Constraint::Length(14),
                    Constraint::Length(14),
                    Constraint::Length(14),
                ],
            )
            .header(
                Row::new(vec!["Run", "State", "Team", "History"]).style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            )
            .block(block)
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 45, 58)))
            .highlight_symbol("› "),
            area,
            &mut state,
        );
    }

    fn render_empty(&self, frame: &mut Frame<'_>, area: Rect, title: &str, message: &str) {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::raw(message),
            ])
            .wrap(Wrap { trim: true })
            .block(panel(title, None)),
            area,
        );
    }

    fn render_overlay(&self, frame: &mut Frame<'_>, area: Rect, overlay: Overlay) {
        let width = area.width.saturating_sub(10).clamp(30, 100);
        let height = area.height.saturating_sub(6).clamp(8, 30);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        let popup = Rect {
            x,
            y,
            width,
            height,
        };
        frame.render_widget(Clear, popup);
        match overlay {
            Overlay::Welcome => self.render_welcome(frame, popup),
            Overlay::Help => self.render_help(frame, popup),
            Overlay::Detail(target) => self.render_detail(frame, popup, target),
        }
    }

    fn render_welcome(&self, frame: &mut Frame<'_>, area: Rect) {
        let presentation = self.snapshot.presentation.as_ref();
        let lines = vec![
            Line::styled(
                "Welcome to Orynth",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("Orynth manages teams of AI agents."),
            Line::raw(""),
            Line::styled(
                presentation.map_or(
                    "This offline demo is a real recovered runtime projection.",
                    |value| value.description.as_str(),
                ),
                Style::default().fg(Color::Gray),
            ),
            Line::raw(""),
            Line::raw("Coordinator manages the work"),
            Line::raw("AUTH-01 handles authentication"),
            Line::raw("DB-02 checks the database"),
            Line::raw("SEC-03 reviews security"),
            Line::raw(""),
            Line::styled(
                "Use number keys or Tab to explore. Enter continues.",
                Style::default().fg(Color::Yellow),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel("FIRST RUN", Some("A compact guide to the demo"))),
            area,
        );
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![
            Line::styled(
                format!("{}  —  {}", self.screen.label(), self.screen.subtitle()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw(screen_explanation(self.screen)),
            Line::raw(""),
            Line::styled(
                "Controls",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::raw("↑ ↓ / j k   Move selection"),
            Line::raw("Tab         Next section"),
            Line::raw("/           Filter the current list"),
            Line::raw("r           Refresh authoritative state"),
            Line::raw("Esc         Close this panel"),
            Line::raw("q / Ctrl-C  Quit"),
        ];
        lines.insert(
            5,
            Line::raw(if self.current_item_count() > 0 {
                "Enter       Open human + technical details"
            } else {
                "Enter       Unavailable: nothing to inspect"
            }),
        );
        if self.screen == Screen::Activity {
            lines.insert(9, Line::raw("[ ]         Older/newest Activity pages"));
        }
        if self.screen == Screen::Runs {
            lines.insert(9, Line::raw("s           Select a persisted run"));
        }
        if self.screen == Screen::Conflicts {
            lines.insert(9, Line::raw("a           Toggle Conflicts/Assumptions"));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(
                "HELP",
                Some("Keys and the meaning of the current screen"),
            )),
            area,
        );
    }

    fn render_detail(&self, frame: &mut Frame<'_>, area: Rect, target: DetailTarget) {
        let (title, lines) = self.detail_lines(target);
        let paragraph = Paragraph::new(lines)
            .scroll((self.detail_scroll, 0))
            .wrap(Wrap { trim: true })
            .block(panel(
                &title,
                Some("Human summary  •  Technical Details  •  Esc closes"),
            ));
        frame.render_widget(paragraph, area);
    }

    fn detail_lines(&self, target: DetailTarget) -> (String, Vec<Line<'static>>) {
        let Some(recovered) = self.recovered() else {
            return ("Details".to_owned(), vec![Line::raw("No run is selected.")]);
        };
        match target {
            DetailTarget::Agent(index) | DetailTarget::Permission(index) => {
                let id = self.agent_ids().get(index).copied();
                let Some(id) = id else {
                    return (
                        "Agent details".to_owned(),
                        vec![Line::raw("No agent selected.")],
                    );
                };
                let Some(agent) = recovered.manager.agents.get(&id) else {
                    return ("Agent details".to_owned(), vec![]);
                };
                let (lifecycle, health) = agent_status(agent.status, agent.health.status);
                let mut lines = vec![
                    Line::styled(
                        format!("{} — {}", agent.name, agent_role(agent)),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("{lifecycle} · {health}")),
                    Line::raw(format!("Mission: {}", agent.mission)),
                    Line::raw(format!(
                        "Model: {} ({})",
                        model_name(&agent.model),
                        model_class(&agent.model.class)
                    )),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Logical AgentId: {}", agent.agent_id)),
                    Line::raw(format!("Provider: {}", agent.model.provider)),
                    Line::raw(format!("Model ID: {}", agent.model.model)),
                    Line::raw(format!("Lifecycle enum: {:?}", agent.status)),
                    Line::raw(format!("Health enum: {:?}", agent.health.status)),
                    Line::raw(format!("Budget: {}", budget_summary(agent.budget))),
                    Line::raw(format!(
                        "Owned resources: {}",
                        join_or_none(&agent.owned_resources)
                    )),
                    Line::raw(format!(
                        "Assumptions: {}  Conflicts: {}  Failures: {}",
                        agent.assumption_ids.len(),
                        agent.conflict_ids.len(),
                        agent.failure_ids.len()
                    )),
                ];
                if let Some(profile) = &agent.specialist {
                    lines.extend([
                        Line::raw(format!("Role profile: {}", profile.role)),
                        Line::raw(format!("Scope: {}", join_or_none(&profile.scope))),
                        Line::raw(format!(
                            "Subscriptions: {}",
                            join_or_none(&profile.subscriptions)
                        )),
                    ]);
                }
                (format!("{} INSPECTOR", agent.name), lines)
            }
            DetailTarget::Event(index) => {
                let event = self.event_records().get(index);
                let Some(event) = event else {
                    return ("Activity details".to_owned(), vec![]);
                };
                let view = human_event(recovered, event);
                let lines = vec![
                    Line::styled(
                        view.title,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(view.summary),
                    Line::raw(""),
                    Line::styled(
                        "Why it matters",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(view.why),
                    Line::raw(format!("Involved: {}", join_or_none(&view.involved))),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Event ID: {}", event.event.id)),
                    Line::raw(format!("Sequence: {}", event.sequence)),
                    Line::raw(format!("Occurred: {} ms", event.event.occurred_at_ms)),
                    Line::raw(format!("Run ID: {}", event.event.run_id)),
                    Line::raw(format!("Event kind: {}", view.kind)),
                    Line::raw(format!(
                        "Raw payload: {}",
                        bounded(&format!("{:?}", event.event.kind), MAX_DETAIL_CHARS)
                    )),
                ];
                ("ACTIVITY INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Message(index) => {
                let message = recovered.messages.get(index);
                let Some(message) = message else {
                    return ("Message details".to_owned(), vec![]);
                };
                let view = message_presentation(recovered, message);
                let lines = vec![
                    Line::styled(
                        format!("{}  {}", view.route, view.kind),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        view.title,
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(view.body),
                    Line::raw(""),
                    Line::styled(view.why, Style::default().fg(Color::Gray)),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Message ID: {}", message.id)),
                    Line::raw(format!("Sender: {}", message.sender)),
                    Line::raw(format!("Recipient: {}", message.recipient)),
                    Line::raw(format!("Provenance: {:?}", message.provenance)),
                    Line::raw(format!(
                        "Trust origin: {:?}",
                        message.effective_trust_origin()
                    )),
                    Line::raw(format!(
                        "Raw typed message: {}",
                        bounded(&format!("{:?}", message.payload), MAX_DETAIL_CHARS)
                    )),
                ];
                ("MESSAGE INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Knowledge(index) => {
                let principal = self
                    .selected_agent_id()
                    .map_or(ContextPrincipal::Runtime, ContextPrincipal::Agent);
                let projection = recovered.context.project(
                    principal,
                    &ProjectionRequest {
                        namespace_patterns: vec!["*".to_owned()],
                        max_blocks: Some(MAX_CONTEXT_ROWS),
                        max_tokens: Some(16_384),
                        include_stale: true,
                        trust_policy: ContextTrustPolicy::AllowAll,
                    },
                );
                let Some(item) = projection.blocks.get(index) else {
                    return ("Knowledge details".to_owned(), vec![]);
                };
                let block = &item.block;
                let (life, explanation) = context_lifecycle(block.lifecycle);
                let lines = vec![
                    Line::styled(
                        knowledge_name(&block.namespace),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("{life}: {explanation}")),
                    Line::raw(format!(
                        "Revision: {}  Tokens: {}  Importance: {}",
                        block.revision, block.token_estimate, block.importance
                    )),
                    Line::raw(format!(
                        "Scope: {:?}  Trust: {:?}",
                        block.scope, block.trust
                    )),
                    Line::raw(format!(
                        "Dependencies: {}",
                        block
                            .dependencies
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Namespace: {}", block.namespace)),
                    Line::raw(format!("ContextBlockId: {}", block.id)),
                    Line::raw(format!("Content hash: {:?}", block.content_hash)),
                    Line::raw(format!("Pinned: {}", block.pinned)),
                    Line::raw(format!("Created event: {:?}", block.created_event)),
                ];
                ("KNOWLEDGE INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Tool(index) => {
                let record = recovered.tools.records().values().nth(index);
                let Some(record) = record else {
                    return ("Tool details".to_owned(), vec![]);
                };
                let lines = vec![
                    Line::styled(
                        format!(
                            "{} — {}",
                            tool_name(&record.proposal.tool_name),
                            tool_state_label(record.state)
                        ),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!(
                        "Agent: {}",
                        agent_label(recovered, record.proposal.agent_id)
                    )),
                    Line::raw(format!(
                        "Result: {}",
                        record
                            .detail
                            .as_deref()
                            .unwrap_or("No additional result recorded.")
                    )),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Transaction ID: {}", record.transaction_id)),
                    Line::raw(format!("Tool ID: {}", record.proposal.tool_name)),
                    Line::raw(format!("Input: {:?}", record.proposal.input)),
                    Line::raw(format!("Provenance: {:?}", record.proposal.provenance)),
                    Line::raw(format!("State: {:?}", record.state)),
                    Line::raw(format!("Repair: {:?}", record.repair)),
                    Line::raw(format!("Preview: {:?}", record.preview)),
                ];
                ("TOOL INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Conflict(index) => {
                let conflict = recovered.assumptions.conflicts().values().nth(index);
                let Some(conflict) = conflict else {
                    return ("Conflict details".to_owned(), vec![]);
                };
                let left = recovered.assumptions.assumptions().get(&conflict.left);
                let right = recovered.assumptions.assumptions().get(&conflict.right);
                let mut lines = vec![Line::styled(
                    format!("Agents disagree about {}", conflict.subject),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )];
                for assumption in [left, right].into_iter().flatten() {
                    lines.push(Line::raw(format!(
                        "{} believes {}",
                        agent_label(recovered, assumption.owner),
                        assumption.normalized_value
                    )));
                }
                lines.extend([
                    Line::raw("Status: Unresolved"),
                    Line::raw(format!(
                        "Affected: {}",
                        conflict
                            .affected_agents
                            .iter()
                            .map(|id| agent_label(recovered, *id))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Conflict ID: {}", conflict.id)),
                    Line::raw(format!("Left assumption: {}", conflict.left)),
                    Line::raw(format!("Right assumption: {}", conflict.right)),
                ]);
                ("CONFLICT INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Assumption(index) => {
                let assumption = recovered.assumptions.assumptions().values().nth(index);
                let Some(assumption) = assumption else {
                    return ("Assumption details".to_owned(), vec![]);
                };
                let lines = vec![
                    Line::styled(
                        format!(
                            "{} assumes {} is {}",
                            agent_label(recovered, assumption.owner),
                            assumption.subject,
                            assumption.normalized_value
                        ),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(assumption.claim.clone()),
                    Line::raw(format!(
                        "Status: {}",
                        assumption_state_label(assumption.state)
                    )),
                    Line::raw(format!("Evidence: {}", join_or_none(&assumption.evidence))),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Assumption ID: {}", assumption.id)),
                    Line::raw(format!("Subject: {}", assumption.subject)),
                    Line::raw(format!("Normalized value: {}", assumption.normalized_value)),
                    Line::raw(format!("Revision: {}", assumption.revision)),
                    Line::raw(format!("Trust: {:?}", assumption.trust)),
                    Line::raw(format!(
                        "Confidence millis: {}",
                        assumption.confidence_millis
                    )),
                ];
                ("ASSUMPTION INSPECTOR".to_owned(), lines)
            }
            DetailTarget::Run(index) => {
                let Some(run) = self.snapshot.runs.get(index) else {
                    return ("Run details".to_owned(), vec![]);
                };
                let title = run
                    .display_name
                    .clone()
                    .or_else(|| {
                        self.snapshot
                            .presentation
                            .as_ref()
                            .filter(|_| {
                                self.snapshot
                                    .selected
                                    .as_ref()
                                    .is_some_and(|selected| selected.run_id == run.run_id)
                            })
                            .map(|value| value.title.clone())
                    })
                    .unwrap_or_else(|| format!("Persisted session {}", index + 1));
                let lines = vec![
                    Line::styled(
                        title,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("State: {}", run_summary_status_label(&run.status))),
                    Line::raw(format!("History: {} events", run.event_count)),
                    Line::raw(""),
                    Line::styled(
                        "Technical Details",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::raw(format!("Run ID: {}", run.run_id)),
                    Line::styled(
                        "Press s to select this run for inspection.",
                        Style::default().fg(Color::Gray),
                    ),
                ];
                ("RUN INSPECTOR".to_owned(), lines)
            }
        }
    }
}

pub(super) struct TerminalGuard;

impl TerminalGuard {
    pub(super) fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(error) = execute!(
            io::stdout(),
            EnableBracketedPaste,
            EnterAlternateScreen,
            crossterm::cursor::Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            crossterm::cursor::Show,
            LeaveAlternateScreen
        );
    }
}

pub fn run_fullscreen<S: TuiDataSource>(mut source: S) -> Result<(), String> {
    let snapshot = source.snapshot()?;
    let mut app = TuiApp::new(snapshot);
    let _guard =
        TerminalGuard::enter().map_err(|error| format!("could not enter terminal UI: {error}"))?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal =
        Terminal::new(backend).map_err(|error| format!("could not create terminal UI: {error}"))?;
    loop {
        terminal
            .draw(|frame| app.render(frame))
            .map_err(|error| format!("could not render terminal UI: {error}"))?;
        if event::poll(Duration::from_millis(250))
            .map_err(|error| format!("terminal input failed: {error}"))?
            && let Event::Key(key) =
                event::read().map_err(|error| format!("terminal input failed: {error}"))?
            && app.handle_key(key, &mut source)?
        {
            break;
        }
    }
    Ok(())
}

pub fn render_snapshot_for_terminal(snapshot: &TuiSnapshot, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width.max(1), height.max(1));
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    let app = TuiApp::new(snapshot.clone());
    terminal
        .draw(|frame| app.render(frame))
        .expect("test terminal should render");
    let buffer = terminal.backend().buffer();
    (0..height.max(1))
        .map(|y| {
            (0..width.max(1))
                .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn panel(title: &str, subtitle: Option<&str>) -> Block<'static> {
    let title = subtitle.map_or_else(
        || title.to_owned(),
        |subtitle| format!(" {title}  ·  {subtitle} "),
    );
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(THEME.muted))
        .style(Style::default().bg(THEME.surface))
        .title(title)
}
fn health_style(health: HealthStatus) -> Style {
    Style::default().fg(match health {
        HealthStatus::Healthy => Color::Green,
        HealthStatus::Degraded => Color::Yellow,
        HealthStatus::Blocked => Color::Red,
    })
}

fn event_severity_style(severity: &EventSeverity) -> Style {
    Style::default().fg(match severity {
        EventSeverity::Info => THEME.info,
        EventSeverity::Success => THEME.success,
        EventSeverity::Warning => THEME.warning,
        EventSeverity::Danger => THEME.danger,
    })
}
fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Active => "Running",
        RunStatus::Completed => "Finished",
        RunStatus::Cancelled => "Cancelled",
        RunStatus::Failed => "Failed",
    }
}
fn run_summary_status_label(status: &str) -> &str {
    match status {
        "ACTIVE" => "Running",
        "COMPLETED" => "Finished",
        "CANCELLED" => "Cancelled",
        "FAILED" => "Failed",
        "EMPTY" => "Empty",
        other => other,
    }
}
fn assumption_state_label(state: orynth_assumptions::AssumptionState) -> &'static str {
    match state {
        orynth_assumptions::AssumptionState::Active => "Current",
        orynth_assumptions::AssumptionState::Conflicted => "Conflicted",
        orynth_assumptions::AssumptionState::Invalidated => "Invalid",
        orynth_assumptions::AssumptionState::Resolved => "Resolved",
    }
}
fn tool_state_style(state: orynth_tool_runtime::ToolState) -> Style {
    Style::default().fg(match state {
        orynth_tool_runtime::ToolState::Verified | orynth_tool_runtime::ToolState::Committed => {
            Color::Green
        }
        orynth_tool_runtime::ToolState::AwaitingApproval => Color::Yellow,
        orynth_tool_runtime::ToolState::Failed | orynth_tool_runtime::ToolState::Rejected => {
            Color::Red
        }
        _ => Color::White,
    })
}
fn scope_label(scope: orynth_context::ContextScope) -> String {
    match scope {
        orynth_context::ContextScope::Global => "Shared globally".to_owned(),
        orynth_context::ContextScope::Team => "Shared with the team".to_owned(),
        orynth_context::ContextScope::Private(agent_id) => {
            format!("Private to {agent_id}")
        }
    }
}
fn domain_label(domain: orynth_security::CapabilityDomain) -> &'static str {
    match domain {
        orynth_security::CapabilityDomain::Filesystem => "file access",
        orynth_security::CapabilityDomain::Process => "process execution",
        orynth_security::CapabilityDomain::Network => "network access",
        orynth_security::CapabilityDomain::Secrets => "secret access",
        orynth_security::CapabilityDomain::Plugins => "plugin access",
        orynth_security::CapabilityDomain::ExternalServices => "external service access",
    }
}
fn screen_explanation(screen: Screen) -> &'static str {
    match screen {
        Screen::Overview => {
            "A compact control-room summary of the team, warnings, recent activity, and selected agent."
        }
        Screen::Agents => {
            "Each card explains who an agent is responsible for, which model powers it, and whether it needs attention."
        }
        Screen::Activity => {
            "Every row is a human-readable explanation of a recorded runtime event. Raw event kinds remain in details."
        }
        Screen::Knowledge => {
            "The project information visible to the selected agent, with lifecycle and visibility explained in plain language."
        }
        Screen::Messages => {
            "Typed agent-to-agent messages are shown as conversations first; payload and provenance are technical details."
        }
        Screen::Tools => {
            "Recorded tool transactions show intended action, result, risk boundary, and verification without executing anything."
        }
        Screen::Permissions => {
            "Scoped permissions and ownership explain what each agent may access. A lease is not general trust."
        }
        Screen::Conflicts => {
            "Deterministic assumption disagreements are shown side by side so their impact is understandable."
        }
        Screen::Runs => {
            "Persisted sessions are listed as selectable runtime histories. Display names never replace technical identity."
        }
    }
}
fn budget_summary(budget: Option<BudgetState>) -> String {
    budget.map_or_else(
        || "Not configured".to_owned(),
        |value| format!("{} tokens used", value.usage.tokens),
    )
}
fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none recorded".to_owned()
    } else {
        values.join(", ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use orynth_assumptions::{Assumption, AssumptionGraph};
    use orynth_cache::CacheTelemetry;
    use orynth_context::{
        ContextDraft, ContextGraph, ContextKind, ContextOwner, ContextProprioception, ContextScope,
    };
    use orynth_event_store::{AgentState, AgentStatus, RunStatus, RuntimeState};
    use orynth_failure_memory::FailureMemory;
    use orynth_ipc::{IpcEnvelope, IpcMessage};
    use orynth_kernel::{
        AgentId, AgentIdentity, Event, EventKind, ModelClass, ModelRef, ToolTransactionId,
        TrustOrigin,
    };
    use orynth_scheduler::SchedulerState;
    use orynth_security::CapabilityPolicy;
    use orynth_specialist::SpecialistRegistry;
    use orynth_tool_runtime::{
        ToolHistory, ToolProposal, ToolProvenance, ToolState, ToolTransition,
    };
    use std::collections::BTreeMap;

    fn empty_snapshot() -> TuiSnapshot {
        let run_id = RunId::from_u64(700);
        TuiSnapshot {
            selected: Some(RecoveredRun {
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
            }),
            runs: vec![RunSummary {
                run_id,
                display_name: None,
                status: "ACTIVE".to_owned(),
                event_count: 1,
            }],
            presentation: None,
        }
    }

    #[test]
    fn frame_rendering_handles_small_terminal() {
        let text = render_snapshot_for_terminal(
            &TuiSnapshot {
                selected: None,
                runs: Vec::new(),
                presentation: None,
            },
            12,
            6,
        );
        assert!(text.contains("Terminal"));
    }

    #[test]
    fn frame_rendering_is_bounded_and_has_real_identity() {
        let text = render_snapshot_for_terminal(&empty_snapshot(), 100, 30);
        assert!(text.contains("AI Agent Runtime"));
        assert!(text.contains("AI TEAM"));
        assert!(text.contains("RECENT ACTIVITY"));
        assert!(!text.contains("run-00000000000002bc"));
        assert!(text.lines().count() <= 30);
    }

    #[test]
    fn empty_navigation_does_not_escape_bounds() {
        let mut app = TuiApp::new(TuiSnapshot {
            selected: None,
            runs: Vec::new(),
            presentation: None,
        });
        app.move_selection(1);
        app.move_to_end();
        assert_eq!(app.selected_agent, 0);
        assert_eq!(app.selected_run, 0);
    }

    #[test]
    fn every_inspectable_screen_opens_a_detail_target() {
        let mut snapshot = empty_snapshot();
        let run = snapshot.selected.as_mut().expect("fixture run");
        let agent_id = AgentId::from_u64(1);
        let model = ModelRef::new(
            "demo",
            "migration-specialist",
            ModelClass::Custom("demo".to_owned()),
        );
        run.state.agents.insert(
            agent_id,
            AgentState {
                identity: AgentIdentity {
                    id: agent_id,
                    name: "AUTH-01".to_owned(),
                    mission: "Validate the authentication migration".to_owned(),
                    model: model.clone(),
                },
                status: AgentStatus::Running,
                model,
                chunks_received: 1,
                usage: Default::default(),
            },
        );
        run.manager.agents.insert(
            agent_id,
            orynth_runtime::ManagerAgentProjection {
                agent_id,
                name: "AUTH-01".to_owned(),
                mission: "Validate the authentication migration".to_owned(),
                model: ModelRef::new(
                    "demo",
                    "migration-specialist",
                    ModelClass::Custom("demo".to_owned()),
                ),
                status: AgentStatus::Running,
                chunks_received: 1,
                usage: Default::default(),
                parent_id: None,
                child_ids: Vec::new(),
                health: Default::default(),
                budget: None,
                owned_resources: Vec::new(),
                assumption_ids: Vec::new(),
                assumption_origins: Vec::new(),
                conflict_ids: Vec::new(),
                failure_ids: Vec::new(),
                active_failure_ids: Vec::new(),
                specialist: None,
            },
        );
        let mut second_agent = run
            .manager
            .agents
            .get(&agent_id)
            .expect("first manager agent")
            .clone();
        second_agent.agent_id = AgentId::from_u64(2);
        second_agent.name = "DB-02".to_owned();
        second_agent.mission = "Check database migration constraints".to_owned();
        run.manager
            .agents
            .insert(second_agent.agent_id, second_agent);
        run.messages.push(IpcEnvelope::new(
            run.run_id,
            None,
            AgentId::from_u64(1),
            AgentId::from_u64(2),
            IpcMessage::Question {
                subject: "migration".to_owned(),
                why: "confirm the contract".to_owned(),
            },
        ));
        run.context
            .publish(ContextDraft::new(
                "project.schema",
                ContextKind::Contract,
                ContextOwner::Runtime,
                ContextScope::Global,
                "users.id is a UUID",
            ))
            .expect("context fixture");
        run.context
            .publish(ContextDraft::new(
                "project.database",
                ContextKind::Contract,
                ContextOwner::Runtime,
                ContextScope::Global,
                "users table uses the shared migration contract",
            ))
            .expect("second context fixture");
        run.assumptions
            .publish(Assumption::new(
                run.run_id,
                AgentId::from_u64(1),
                "users.id",
                "UUID",
                "the identifier is a UUID",
            ))
            .expect("first assumption fixture");
        run.assumptions
            .publish(Assumption::new(
                run.run_id,
                AgentId::from_u64(2),
                "users.id",
                "BIGINT",
                "the identifier is numeric",
            ))
            .expect("conflict fixture");
        run.tools
            .apply(ToolTransition::Proposed {
                transaction_id: ToolTransactionId::from_u64(4),
                proposal: ToolProposal {
                    run_id: run.run_id,
                    task_id: None,
                    agent_id: AgentId::from_u64(1),
                    tool_name: "filesystem.inspect".to_owned(),
                    input: BTreeMap::new(),
                    provenance: ToolProvenance::Agent,
                    input_origins: vec![TrustOrigin::Generated],
                },
                state: ToolState::Validated,
            })
            .expect("tool fixture");

        let mut app = TuiApp::new(snapshot);
        app.screen = Screen::Knowledge;
        app.move_selection(1);
        assert_eq!(app.selected_knowledge, 1);
        assert_eq!(app.selected_agent, 0);
        app.screen = Screen::Permissions;
        app.move_selection(1);
        assert_eq!(app.selected_permission, 1);
        assert_eq!(app.selected_agent, 0);
        app.screen = Screen::Overview;
        app.move_selection(1);
        assert_eq!(app.selected_agent, 1);
        app.screen = Screen::Agents;
        app.move_to_start();
        app.move_selection(1);
        assert_eq!(app.selected_agent, 1);

        struct Source;
        impl TuiDataSource for Source {
            fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
                Ok(TuiSnapshot {
                    selected: None,
                    runs: Vec::new(),
                    presentation: None,
                })
            }

            fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
                Ok(())
            }
        }
        app.screen = Screen::Activity;
        app.selected_event = 0;
        app.open_detail();
        let mut source = Source;
        app.handle_key(KeyEvent::from(KeyCode::Down), &mut source)
            .expect("detail scroll");
        assert_eq!(app.detail_scroll, 1);
        app.overlay = None;
        for (screen, expected) in [
            (Screen::Agents, DetailTarget::Agent(0)),
            (Screen::Activity, DetailTarget::Event(0)),
            (Screen::Messages, DetailTarget::Message(0)),
            (Screen::Knowledge, DetailTarget::Knowledge(0)),
            (Screen::Tools, DetailTarget::Tool(0)),
            (Screen::Permissions, DetailTarget::Permission(0)),
            (Screen::Conflicts, DetailTarget::Conflict(0)),
            (Screen::Runs, DetailTarget::Run(0)),
        ] {
            app.screen = screen;
            app.selected_agent = 0;
            app.selected_event = 0;
            app.selected_message = 0;
            app.selected_knowledge = 0;
            app.selected_tool = 0;
            app.selected_permission = 0;
            app.selected_conflict = 0;
            app.show_assumptions = false;
            app.open_detail();
            assert_eq!(app.overlay, Some(Overlay::Detail(expected)));
            app.overlay = None;
        }
    }

    #[test]
    fn advertised_global_controls_have_behavior() {
        struct Source;
        impl TuiDataSource for Source {
            fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
                Ok(TuiSnapshot {
                    selected: None,
                    runs: Vec::new(),
                    presentation: None,
                })
            }

            fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
                Ok(())
            }
        }
        let mut app = TuiApp::new(TuiSnapshot {
            selected: None,
            runs: Vec::new(),
            presentation: None,
        });
        let mut source = Source;
        app.handle_key(KeyEvent::from(KeyCode::Char('?')), &mut source)
            .expect("help key");
        assert_eq!(app.overlay, Some(Overlay::Help));
        app.handle_key(KeyEvent::from(KeyCode::Esc), &mut source)
            .expect("escape key");
        assert_eq!(app.overlay, None);
        app.handle_key(KeyEvent::from(KeyCode::Tab), &mut source)
            .expect("tab key");
        assert_eq!(app.screen, Screen::Agents);
        app.screen = Screen::Runs;
        app.handle_key(KeyEvent::from(KeyCode::Tab), &mut source)
            .expect("tab wraps forward");
        assert_eq!(app.screen, Screen::Overview);
        app.handle_key(KeyEvent::from(KeyCode::BackTab), &mut source)
            .expect("tab wraps backward");
        assert_eq!(app.screen, Screen::Runs);
        assert!(
            app.handle_key(KeyEvent::from(KeyCode::Char('q')), &mut source)
                .expect("quit key")
        );
    }
}
