//! Conversation-first projection of the same run used by Advanced Debugger.
//! Offline input is limited to local inspection commands until a durable
//! coordinator conversation contract and provider are available.

use std::{io, time::Duration};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use orynth_context::{ContextPrincipal, ContextTrustPolicy, ProjectionRequest};
use orynth_kernel::{AgentId, EventKind};
use orynth_runtime::RecoveredRun;
use ratatui::{
    Frame, Terminal,
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    fullscreen::{TerminalGuard, TuiApp, TuiDataSource, TuiSnapshot},
    presentation::{
        agent_label, agent_role, agent_status, context_lifecycle, human_event, knowledge_name,
        message_presentation, model_name, tool_name,
    },
};

const TEXT: Color = Color::Rgb(231, 238, 241);
const MUTED: Color = Color::Rgb(132, 151, 162);
const BRAND: Color = Color::Rgb(92, 202, 216);
const ACCENT: Color = Color::Rgb(117, 179, 231);
const WARNING: Color = Color::Rgb(239, 185, 94);
const SURFACE: Color = Color::Rgb(24, 35, 44);
const MAX_CONVERSATION_EVENTS: usize = 4;
const MAX_INPUT_CHARS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Input,
    Team,
    Conversation,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Input => Self::Team,
            Self::Team => Self::Conversation,
            Self::Conversation => Self::Input,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Input => Self::Conversation,
            Self::Team => Self::Input,
            Self::Conversation => Self::Team,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentTab {
    Work,
    Conversation,
    Context,
    Tools,
    Access,
}

impl AgentTab {
    const ALL: [Self; 5] = [
        Self::Work,
        Self::Conversation,
        Self::Context,
        Self::Tools,
        Self::Access,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::Conversation => "Conversation",
            Self::Context => "Context",
            Self::Tools => "Tools",
            Self::Access => "Access",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    Help,
    Palette,
    Issue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaletteAction {
    Coordinator,
    OpenSelectedAgent,
    Team,
    Issues,
    ToolActivity,
    RunHistory,
    Debugger,
    Help,
}

impl PaletteAction {
    const ALL: [Self; 8] = [
        Self::Coordinator,
        Self::OpenSelectedAgent,
        Self::Team,
        Self::Issues,
        Self::ToolActivity,
        Self::RunHistory,
        Self::Debugger,
        Self::Help,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Coordinator => "Open Coordinator",
            Self::OpenSelectedAgent => "Open selected agent",
            Self::Team => "Focus AI Team",
            Self::Issues => "View current issues",
            Self::ToolActivity => "View agent tools",
            Self::RunHistory => "View run history",
            Self::Debugger => "Advanced Debugger",
            Self::Help => "Help",
        }
    }
}

#[derive(Clone, Debug)]
struct Workspace {
    snapshot: TuiSnapshot,
    focus: Focus,
    agent: Option<AgentId>,
    selected_team: usize,
    tab: AgentTab,
    input: String,
    history: Vec<String>,
    history_index: Option<usize>,
    overlay: Option<Overlay>,
    palette_index: usize,
    scroll: u16,
    pinned_scroll: bool,
    notice: Option<String>,
}

impl Workspace {
    fn new(snapshot: TuiSnapshot) -> Self {
        Self {
            snapshot,
            focus: Focus::Input,
            agent: None,
            selected_team: 0,
            tab: AgentTab::Work,
            input: String::new(),
            history: Vec::new(),
            history_index: None,
            overlay: None,
            palette_index: 0,
            scroll: 0,
            pinned_scroll: false,
            notice: None,
        }
    }

    fn run(&self) -> Option<&RecoveredRun> {
        self.snapshot.selected.as_ref()
    }

    fn team(&self) -> Vec<AgentId> {
        self.run()
            .map(|run| {
                let mut agents = run.manager.agents.values().collect::<Vec<_>>();
                agents.sort_by_key(|agent| (agent.parent_id.is_some(), agent.name.clone()));
                agents.iter().take(64).map(|agent| agent.agent_id).collect()
            })
            .unwrap_or_default()
    }

    fn selected_agent(&self) -> Option<AgentId> {
        self.team().get(self.selected_team).copied()
    }

    fn open_selected_agent(&mut self) {
        self.agent = self.selected_agent();
        self.tab = AgentTab::Work;
        self.focus = Focus::Conversation;
        self.scroll = 0;
        self.pinned_scroll = false;
    }

    fn coordinator(&mut self) {
        self.agent = None;
        self.focus = Focus::Input;
        self.scroll = 0;
        self.pinned_scroll = false;
    }

    fn refresh<S: TuiDataSource>(&mut self, source: &mut S) {
        match source.snapshot() {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.selected_team = self.selected_team.min(self.team().len().saturating_sub(1));
                if self.agent.is_some_and(|id| {
                    self.run()
                        .is_none_or(|run| !run.manager.agents.contains_key(&id))
                }) {
                    self.coordinator();
                }
            }
            Err(error) => self.notice = Some(format!("Refresh failed: {error}")),
        }
    }

    fn key<S: TuiDataSource>(
        &mut self,
        key: KeyEvent,
        source: &mut S,
    ) -> Result<WorkspaceSignal, String> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(match key.code {
                KeyCode::Char('c') => WorkspaceSignal::Quit,
                KeyCode::Char('k') => {
                    self.overlay = Some(Overlay::Palette);
                    WorkspaceSignal::Stay
                }
                KeyCode::Char('l') => {
                    self.coordinator();
                    WorkspaceSignal::Stay
                }
                _ => WorkspaceSignal::Stay,
            });
        }
        if let Some(overlay) = self.overlay {
            match overlay {
                Overlay::Palette => match key.code {
                    KeyCode::Esc => self.overlay = None,
                    KeyCode::Up => self.palette_index = self.palette_index.saturating_sub(1),
                    KeyCode::Down => {
                        self.palette_index =
                            (self.palette_index + 1).min(PaletteAction::ALL.len() - 1)
                    }
                    KeyCode::Enter => return Ok(self.execute_palette()),
                    _ => {}
                },
                Overlay::Help | Overlay::Issue => {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')) {
                        self.overlay = None;
                    }
                }
            }
            return Ok(WorkspaceSignal::Stay);
        }
        if key.code == KeyCode::Tab {
            self.focus = self.focus.next();
            return Ok(WorkspaceSignal::Stay);
        }
        if key.code == KeyCode::BackTab {
            self.focus = self.focus.previous();
            return Ok(WorkspaceSignal::Stay);
        }
        if key.code == KeyCode::Esc {
            self.coordinator();
            return Ok(WorkspaceSignal::Stay);
        }
        if key.code == KeyCode::Char('?') && (self.focus != Focus::Input || self.input.is_empty()) {
            self.overlay = Some(Overlay::Help);
            return Ok(WorkspaceSignal::Stay);
        }
        match self.focus {
            Focus::Input => {
                if self.agent.is_none() {
                    self.input_key(key);
                }
            }
            Focus::Team => match key.code {
                KeyCode::Up => self.selected_team = self.selected_team.saturating_sub(1),
                KeyCode::Down => {
                    self.selected_team =
                        (self.selected_team + 1).min(self.team().len().saturating_sub(1))
                }
                KeyCode::Enter => self.open_selected_agent(),
                KeyCode::Char('!') if self.issue_count() > 0 => self.overlay = Some(Overlay::Issue),
                KeyCode::Char('q') => return Ok(WorkspaceSignal::Quit),
                _ => {}
            },
            Focus::Conversation => match key.code {
                KeyCode::Up | KeyCode::PageUp => {
                    self.scroll =
                        self.scroll
                            .saturating_add(if key.code == KeyCode::PageUp { 8 } else { 1 });
                    self.pinned_scroll = true;
                }
                KeyCode::Down | KeyCode::PageDown => {
                    self.scroll = self
                        .scroll
                        .saturating_sub(if key.code == KeyCode::PageDown { 8 } else { 1 });
                    self.pinned_scroll = self.scroll > 0;
                }
                KeyCode::End => {
                    self.scroll = 0;
                    self.pinned_scroll = false;
                }
                KeyCode::Left if self.agent.is_some() => self.shift_tab(-1),
                KeyCode::Right if self.agent.is_some() => self.shift_tab(1),
                KeyCode::Char('q') => return Ok(WorkspaceSignal::Quit),
                _ => {}
            },
        }
        if key.code == KeyCode::Char('r') && self.focus != Focus::Input {
            self.refresh(source);
        }
        Ok(WorkspaceSignal::Stay)
    }

    fn shift_tab(&mut self, direction: isize) {
        let index = AgentTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let next = (index as isize + direction).clamp(0, 4) as usize;
        self.tab = AgentTab::ALL[next];
        self.scroll = 0;
    }

    fn input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
                self.history_index = None;
            }
            KeyCode::Up => self.history_step(-1),
            KeyCode::Down => self.history_step(1),
            KeyCode::Char(c) if self.input.chars().count() < MAX_INPUT_CHARS => {
                self.input.push(c);
                self.history_index = None;
            }
            _ => {}
        }
    }

    fn history_step(&mut self, direction: isize) {
        if self.history.is_empty() {
            return;
        }
        let index = self.history_index.unwrap_or(self.history.len());
        let next = (index as isize + direction).clamp(0, self.history.len() as isize) as usize;
        self.history_index = Some(next);
        self.input = self.history.get(next).cloned().unwrap_or_default();
    }

    fn submit(&mut self) {
        let value = self.input.trim().to_owned();
        if value.is_empty() {
            return;
        }
        if self.history.len() == 32 {
            self.history.remove(0);
        }
        self.history.push(value.clone());
        self.history_index = None;
        self.input.clear();
        let mut parts = value.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "/help" => self.overlay = Some(Overlay::Help),
            "/coordinator" => self.coordinator(),
            "/agents" => self.focus = Focus::Team,
            "/status" => self.notice = Some(self.status_text()),
            "/conflicts" => {
                if self.issue_count() > 0 { self.overlay = Some(Overlay::Issue); }
                else { self.notice = Some("No current issues in this run.".to_owned()); }
            }
            "/agent" => {
                let name = parts.next().unwrap_or_default();
                if let Some((index, id)) = self.team().iter().enumerate().find(|(_, id)| {
                    self.run().and_then(|run| run.manager.agents.get(id)).is_some_and(|agent| agent.name.eq_ignore_ascii_case(name))
                }) {
                    self.selected_team = index;
                    self.agent = Some(*id);
                    self.tab = AgentTab::Work;
                    self.focus = Focus::Conversation;
                } else { self.notice = Some(format!("No agent named {name}. Use /agents.")); }
            }
            "/tools" => { self.open_selected_agent(); self.tab = AgentTab::Tools; }
            "/context" => { self.open_selected_agent(); self.tab = AgentTab::Context; }
            "/runs" => self.notice = Some(format!("{} recorded run(s). Open Advanced Debugger for run selection.", self.snapshot.runs.len())),
            "/debug" => self.notice = Some("Opening Advanced Debugger...".to_owned()),
            _ if command.starts_with('/') => self.notice = Some(format!("Unknown command {command}. Use /help.")),
            _ => self.notice = Some("Offline workspace: no Coordinator model is connected. Runtime activity remains available for inspection; your message was not sent or recorded.".to_owned()),
        }
    }

    fn execute_palette(&mut self) -> WorkspaceSignal {
        self.overlay = None;
        match PaletteAction::ALL[self.palette_index] {
            PaletteAction::Coordinator => self.coordinator(),
            PaletteAction::OpenSelectedAgent => self.open_selected_agent(),
            PaletteAction::Team => self.focus = Focus::Team,
            PaletteAction::Issues => {
                if self.issue_count() > 0 {
                    self.overlay = Some(Overlay::Issue)
                } else {
                    self.notice = Some("No current issues.".to_owned())
                }
            }
            PaletteAction::ToolActivity => {
                self.open_selected_agent();
                self.tab = AgentTab::Tools;
            }
            PaletteAction::RunHistory => {
                self.notice = Some(format!(
                    "{} recorded run(s). Advanced Debugger has run selection.",
                    self.snapshot.runs.len()
                ))
            }
            PaletteAction::Debugger => return WorkspaceSignal::Debugger,
            PaletteAction::Help => self.overlay = Some(Overlay::Help),
        }
        WorkspaceSignal::Stay
    }

    fn issue_count(&self) -> usize {
        self.run().map_or(0, |run| {
            run.assumptions.conflicts().len() + run.manager.active_failure_count
        })
    }

    fn status_text(&self) -> String {
        self.run().map_or_else(
            || "No run selected. Open a recorded run in Advanced Debugger.".to_owned(),
            |run| {
                format!(
                    "{} agents · {} issue(s) · {} recorded events",
                    run.manager.agents.len(),
                    self.issue_count(),
                    run.state.events_applied
                )
            },
        )
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < 38 || area.height < 10 {
            frame.render_widget(
                Paragraph::new("ORYNTH\nResize terminal to at least 38x10.\nCtrl+C quits.")
                    .block(Block::default().borders(Borders::ALL)),
                area,
            );
            return;
        }
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(frame, rows[0]);
        if area.width >= 72 {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
                .split(rows[1]);
            self.render_main(frame, cols[0]);
            self.render_team(frame, cols[1]);
        } else if self.focus == Focus::Team {
            self.render_team(frame, rows[1]);
        } else {
            self.render_main(frame, rows[1]);
        }
        self.render_input(frame, rows[2]);
        self.render_footer(frame, rows[3]);
        if let Some(overlay) = self.overlay {
            self.render_overlay(frame, area, overlay);
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = self
            .snapshot
            .presentation
            .as_ref()
            .map(|view| view.title.as_str())
            .or_else(|| {
                self.run().and_then(|run| {
                    run.state
                        .tasks
                        .values()
                        .next()
                        .map(|task| task.title.as_str())
                })
            })
            .unwrap_or("New workspace");
        let summary = self.run().map_or_else(
            || "NO RUN".to_owned(),
            |run| {
                format!(
                    "{} agents / {} issue(s)",
                    run.manager.agents.len(),
                    self.issue_count()
                )
            },
        );
        let lines = vec![Line::from(vec![
            Span::styled(
                " ORYNTH  ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled(title.to_owned(), Style::default().fg(TEXT)),
            Span::styled(format!("  /  {summary}"), Style::default().fg(MUTED)),
        ])];
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::BOTTOM)),
            area,
        );
    }

    fn render_main(&self, frame: &mut Frame<'_>, area: Rect) {
        let title = self
            .agent
            .and_then(|id| self.run().and_then(|run| run.manager.agents.get(&id)))
            .map_or_else(
                || "COORDINATOR".to_owned(),
                |agent| format!("{}  ·  {}", agent.name, agent_role(agent)),
            );
        let mut lines = if let Some(id) = self.agent {
            self.agent_lines(id)
        } else {
            self.coordinator_lines()
        };
        if lines.is_empty() {
            lines.push(Line::raw("No recorded activity yet."));
        }
        let tab_line = if self.agent.is_some() {
            Some(Line::from(
                AgentTab::ALL
                    .iter()
                    .map(|tab| {
                        Span::styled(
                            format!("  {}  ", tab.label()),
                            if *tab == self.tab {
                                Style::default().fg(BRAND).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(MUTED)
                            },
                        )
                    })
                    .collect::<Vec<_>>(),
            ))
        } else {
            None
        };
        let inner = Block::default()
            .borders(Borders::RIGHT)
            .title(format!(" {title} "))
            .border_style(Style::default().fg(if self.focus == Focus::Conversation {
                BRAND
            } else {
                MUTED
            }));
        let inner_area = inner.inner(area);
        frame.render_widget(inner, area);
        let body = if let Some(tabs) = tab_line {
            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(inner_area);
            frame.render_widget(Paragraph::new(tabs), sections[0]);
            sections[1]
        } else {
            inner_area
        };
        let visible = body.height as usize;
        let max_scroll = lines.len().saturating_sub(visible) as u16;
        let offset = if self.pinned_scroll {
            max_scroll.saturating_sub(self.scroll)
        } else {
            max_scroll
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((offset, 0)),
            body,
        );
    }

    fn coordinator_lines(&self) -> Vec<Line<'static>> {
        let Some(run) = self.run() else {
            return vec![
                Line::styled(
                    "Welcome to Orynth",
                    Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::raw(
                    "Talk to one Coordinator while specialist agents work in the background.",
                ),
                Line::raw(""),
                Line::raw(
                    "No run is selected. Offline inspection is available through /help and Ctrl+K.",
                ),
                Line::raw(
                    "A model must be connected before natural-language requests can be sent.",
                ),
            ];
        };
        let mut lines = vec![Line::styled(
            "PROJECT GOAL",
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        )];
        if let Some(task) = run.state.tasks.values().next() {
            lines.push(Line::raw(format!("  {}", task.title)));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Coordinator / recorded runtime activity",
            Style::default().fg(ACCENT),
        ));
        for stored in run
            .events
            .iter()
            .rev()
            .filter(|stored| {
                matches!(
                    stored.event.kind,
                    EventKind::AgentCreated { .. }
                        | EventKind::AgentPaused { .. }
                        | EventKind::AgentResumed { .. }
                        | EventKind::AssumptionTransition { .. }
                        | EventKind::ToolTransition { .. }
                        | EventKind::ModelRequested { .. }
                        | EventKind::ModelFailed { .. }
                        | EventKind::RunCompleted { .. }
                        | EventKind::RunFailed { .. }
                )
            })
            .take(MAX_CONVERSATION_EVENTS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let event = human_event(run, stored);
            lines.push(Line::styled(
                format!("  {}", event.title),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!("  {}", event.summary),
                Style::default().fg(MUTED),
            ));
            lines.push(Line::raw(""));
        }
        if self.pinned_scroll {
            lines.push(Line::styled(
                "↓ New activity below · End to follow",
                Style::default().fg(WARNING),
            ));
        }
        lines
    }

    fn agent_lines(&self, id: AgentId) -> Vec<Line<'static>> {
        let Some(run) = self.run() else {
            return Vec::new();
        };
        let Some(agent) = run.manager.agents.get(&id) else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        match self.tab {
            AgentTab::Work => {
                let (status, health) = agent_status(agent.status, agent.health.status);
                lines.extend([
                    heading("TASK"),
                    Line::raw(agent.mission.clone()),
                    Line::raw(""),
                    heading("STATUS"),
                    Line::raw(format!("{status} · {health}")),
                    Line::raw(""),
                    heading("MODEL"),
                    Line::raw(model_name(&agent.model)),
                    Line::styled(
                        "Agent identity stays the same when its model changes.",
                        Style::default().fg(MUTED),
                    ),
                    Line::raw(""),
                    heading("TEAM"),
                    Line::raw(format!(
                        "Parent: {}",
                        agent.parent_id.map_or_else(
                            || "None · Coordinator".to_owned(),
                            |parent| agent_label(run, parent)
                        )
                    )),
                    Line::raw(format!("Children: {}", agent.child_ids.len())),
                    Line::raw(""),
                    heading("CURRENT ISSUES"),
                    Line::raw(format!(
                        "{} conflict(s) · {} active failure(s)",
                        agent.conflict_ids.len(),
                        agent.active_failure_ids.len()
                    )),
                ]);
            }
            AgentTab::Conversation => {
                lines.push(Line::styled(
                    "Recorded agent exchanges · read only",
                    Style::default().fg(MUTED),
                ));
                lines.push(Line::raw(""));
                for message in run
                    .messages
                    .iter()
                    .filter(|message| message.sender == id || message.recipient == id)
                    .rev()
                    .take(32)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                {
                    let view = message_presentation(run, message);
                    lines.push(heading(&view.route));
                    lines.push(Line::raw(format!("{} · {}", view.kind, view.title)));
                    lines.push(Line::raw(view.body));
                    lines.push(Line::raw(""));
                }
            }
            AgentTab::Context => {
                let projection = run.context.project(
                    ContextPrincipal::Agent(id),
                    &ProjectionRequest {
                        namespace_patterns: vec!["*".to_owned()],
                        max_blocks: Some(32),
                        max_tokens: Some(16_384),
                        include_stale: true,
                        trust_policy: ContextTrustPolicy::AllowAll,
                    },
                );
                for item in projection.blocks {
                    let (status, _) = context_lifecycle(item.block.lifecycle);
                    lines.push(heading(&knowledge_name(&item.block.namespace)));
                    lines.push(Line::raw(format!(
                        "Revision {} · {status}",
                        item.block.revision
                    )));
                    lines.push(Line::raw(""));
                }
                if lines.is_empty() {
                    lines.push(Line::raw("No visible project context for this agent."));
                }
            }
            AgentTab::Tools => {
                for record in run
                    .tools
                    .records()
                    .values()
                    .filter(|record| record.proposal.agent_id == id)
                    .take(32)
                {
                    lines.push(heading(&tool_name(&record.proposal.tool_name)));
                    for (key, value) in record.proposal.input.iter().take(4) {
                        lines.push(Line::raw(format!("{key}: {value}")));
                    }
                    lines.push(Line::raw(format!("Result: {:?}", record.state)));
                    lines.push(Line::raw(""));
                }
                if lines.is_empty() {
                    lines.push(Line::raw("No recorded tool activity."));
                }
            }
            AgentTab::Access => {
                lines.push(heading("OWNED RESOURCES"));
                for resource in agent.owned_resources.iter().take(32) {
                    lines.push(Line::raw(resource.clone()));
                }
                if agent.owned_resources.is_empty() {
                    lines.push(Line::raw("No resources owned."));
                }
                lines.push(Line::raw(""));
                lines.push(heading("DECLARED WORK SCOPE"));
                if let Some(profile) = &agent.specialist {
                    for resource in profile.scope.iter().take(32) {
                        lines.push(Line::raw(resource.clone()));
                    }
                    lines.push(Line::styled(
                        "Declared scope is descriptive; runtime capabilities are authoritative.",
                        Style::default().fg(MUTED),
                    ));
                } else {
                    lines.push(Line::raw("Coordinator · no specialist scope"));
                }
            }
        }
        lines
    }

    fn render_team(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = Vec::new();
        let compact = area.width < 30;
        if let Some(run) = self.run() {
            for (index, id) in self.team().iter().enumerate() {
                let agent = &run.manager.agents[id];
                let selected = index == self.selected_team;
                let marker = if selected { "›" } else { " " };
                lines.push(Line::styled(
                    format!(
                        "{marker} {}",
                        if agent.parent_id.is_none() {
                            "COORDINATOR"
                        } else {
                            &agent.name
                        }
                    ),
                    Style::default()
                        .fg(if selected { BRAND } else { TEXT })
                        .add_modifier(Modifier::BOLD),
                ));
                let role = agent_role(agent);
                let role = if compact {
                    role.split_whitespace().next().unwrap_or("Agent").to_owned()
                } else {
                    role
                };
                lines.push(Line::styled(
                    format!("  {role}"),
                    Style::default().fg(MUTED),
                ));
                let (status, health) = agent_status(agent.status, agent.health.status);
                if compact {
                    let status = if status == "Running" {
                        "Working"
                    } else {
                        status
                    };
                    let marker = if health == "Needs attention" {
                        "!"
                    } else {
                        "·"
                    };
                    lines.push(Line::styled(
                        format!("  {marker} {status} · {}", model_name(&agent.model)),
                        Style::default().fg(if health == "Needs attention" {
                            WARNING
                        } else {
                            MUTED
                        }),
                    ));
                } else {
                    lines.push(Line::styled(
                        format!("  {status} · {health}"),
                        Style::default().fg(if health == "Needs attention" {
                            WARNING
                        } else {
                            MUTED
                        }),
                    ));
                    lines.push(Line::styled(
                        format!("  Model: {}", model_name(&agent.model)),
                        Style::default().fg(MUTED),
                    ));
                    lines.push(Line::raw(""));
                }
            }
            if self.issue_count() > 0 {
                lines.push(heading("ATTENTION"));
                if let Some(conflict) = run.assumptions.conflicts().values().next() {
                    lines.push(Line::styled(
                        format!("! {} disagreement", conflict.subject),
                        Style::default().fg(WARNING),
                    ));
                }
                if !compact {
                    lines.push(Line::styled("  ! opens issue", Style::default().fg(MUTED)));
                }
            }
        } else {
            lines.push(Line::raw("No team selected."));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::LEFT)
                    .title(" AI TEAM ")
                    .border_style(Style::default().fg(if self.focus == Focus::Team {
                        BRAND
                    } else {
                        MUTED
                    }))
                    .style(Style::default().bg(SURFACE)),
            ),
            area,
        );
    }

    fn render_input(&self, frame: &mut Frame<'_>, area: Rect) {
        let content = if self.input.is_empty() {
            "ask the Coordinator anything...".to_owned()
        } else {
            self.input.clone()
        };
        let content = if self.agent.is_some() {
            "Worker view is read only. Esc returns to Coordinator.".to_owned()
        } else {
            content
        };
        let style = Style::default().fg(if self.input.is_empty() || self.agent.is_some() {
            MUTED
        } else {
            TEXT
        });
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" > ", Style::default().fg(BRAND)),
                Span::styled(content, style),
            ]))
            .block(
                Block::default()
                    .borders(Borders::TOP | Borders::BOTTOM)
                    .title(" COORDINATOR INPUT ")
                    .border_style(Style::default().fg(if self.focus == Focus::Input {
                        BRAND
                    } else {
                        MUTED
                    })),
            ),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let message = self
            .notice
            .as_deref()
            .unwrap_or("Tab focus   Enter open/send   Ctrl+K commands   ? help   Ctrl+C quit");
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(MUTED)),
            area,
        );
    }

    fn render_overlay(&self, frame: &mut Frame<'_>, area: Rect, overlay: Overlay) {
        let width = area.width.saturating_sub(4).clamp(1, 72);
        let height = area.height.saturating_sub(2).clamp(1, 22);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let (title, lines) = match overlay {
            Overlay::Help => (
                "HOW ORYNTH WORKS",
                vec![
                    Line::raw("Talk to one Coordinator while it manages specialist AI agents."),
                    Line::raw(""),
                    Line::raw("The main pane shows recorded coordination and worker work."),
                    Line::raw("The AI Team shows each worker, status, health, and model."),
                    Line::raw("Tab focuses the team; Up/Down selects; Enter opens a worker."),
                    Line::raw("Left/Right changes worker tabs. Esc returns to Coordinator."),
                    Line::raw("Ctrl+K opens commands. /help lists this guide."),
                    Line::raw("/agents /agent NAME /status /conflicts /tools /context /runs"),
                    Line::raw("Advanced Debugger exposes raw runtime detail. Ctrl+W returns."),
                    Line::raw(""),
                    Line::raw("Offline mode cannot send natural-language requests to a model."),
                    Line::raw("Esc closes · Ctrl+C quits"),
                ],
            ),
            Overlay::Palette => (
                "COMMANDS",
                PaletteAction::ALL
                    .iter()
                    .enumerate()
                    .map(|(index, action)| {
                        Line::styled(
                            format!(
                                "{} {}",
                                if index == self.palette_index {
                                    "›"
                                } else {
                                    " "
                                },
                                action.label()
                            ),
                            Style::default().fg(if index == self.palette_index {
                                BRAND
                            } else {
                                TEXT
                            }),
                        )
                    })
                    .collect(),
            ),
            Overlay::Issue => ("PROJECT CONFLICT", self.issue_lines()),
        };
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} "))
                    .border_style(Style::default().fg(BRAND))
                    .style(Style::default().bg(SURFACE)),
            ),
            rect,
        );
    }

    fn issue_lines(&self) -> Vec<Line<'static>> {
        let Some(run) = self.run() else {
            return vec![Line::raw("No selected run.")];
        };
        let Some(conflict) = run.assumptions.conflicts().values().next() else {
            return vec![Line::raw("No unresolved conflict.")];
        };
        let left = run.assumptions.assumptions().get(&conflict.left);
        let right = run.assumptions.assumptions().get(&conflict.right);
        let mut lines = vec![
            Line::raw(format!("Agents disagree about {}", conflict.subject)),
            Line::raw(""),
        ];
        if let (Some(left), Some(right)) = (left, right) {
            lines.push(heading(&agent_label(run, left.owner)));
            lines.push(Line::raw(left.normalized_value.clone()));
            lines.push(Line::raw(""));
            lines.push(heading(&agent_label(run, right.owner)));
            lines.push(Line::raw(right.normalized_value.clone()));
        }
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "Different values are recorded for the same project contract.",
        ));
        lines.push(Line::raw(
            "The runtime flagged the affected workers. Esc closes.",
        ));
        lines
    }
}

fn heading(value: &str) -> Line<'static> {
    Line::styled(
        value.to_owned(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceSignal {
    Stay,
    Debugger,
    Quit,
}

pub fn run_workspace<S: TuiDataSource>(mut source: S) -> Result<(), String> {
    let snapshot = source.snapshot()?;
    let mut workspace = Workspace::new(snapshot.clone());
    let mut debugger = TuiApp::embedded(snapshot);
    let mut debug_mode = false;
    let _guard =
        TerminalGuard::enter().map_err(|error| format!("could not enter terminal UI: {error}"))?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|error| format!("could not create terminal UI: {error}"))?;
    loop {
        terminal
            .draw(|frame| {
                if debug_mode {
                    debugger.render(frame)
                } else {
                    workspace.render(frame)
                }
            })
            .map_err(|error| format!("could not render terminal UI: {error}"))?;
        if !event::poll(Duration::from_millis(250))
            .map_err(|error| format!("terminal input failed: {error}"))?
        {
            continue;
        }
        let Event::Key(key) =
            event::read().map_err(|error| format!("terminal input failed: {error}"))?
        else {
            continue;
        };
        if debug_mode {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
                debug_mode = false;
                workspace.refresh(&mut source);
            } else if debugger.handle_key(key, &mut source)? {
                break;
            }
        } else {
            if key.code == KeyCode::Enter
                && workspace.focus == Focus::Input
                && workspace.input.trim() == "/debug"
            {
                workspace.input.clear();
                debug_mode = true;
                debugger = TuiApp::embedded(source.snapshot()?);
                continue;
            }
            match workspace.key(key, &mut source)? {
                WorkspaceSignal::Stay => {}
                WorkspaceSignal::Debugger => {
                    debug_mode = true;
                    debugger = TuiApp::embedded(source.snapshot()?);
                }
                WorkspaceSignal::Quit => break,
            }
        }
    }
    Ok(())
}

pub fn render_workspace_for_terminal(snapshot: &TuiSnapshot, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width.max(1), height.max(1));
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    let workspace = Workspace::new(snapshot.clone());
    terminal
        .draw(|frame| workspace.render(frame))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn empty() -> TuiSnapshot {
        TuiSnapshot {
            selected: None,
            runs: Vec::new(),
            presentation: None,
        }
    }

    #[test]
    fn empty_and_tiny_workspace_are_honest() {
        let empty = empty();
        let normal = render_workspace_for_terminal(&empty, 100, 28);
        assert!(normal.contains("COORDINATOR"));
        assert!(normal.contains("AI TEAM"));
        assert!(normal.contains("ask the Coordinator"));
        let tiny = render_workspace_for_terminal(&empty, 30, 8);
        assert!(tiny.contains("Resize terminal"));
    }

    #[test]
    fn input_and_history_are_focus_aware() {
        let mut app = Workspace::new(empty());
        for ch in "/status".chars() {
            app.input_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        app.input_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            app.notice
                .as_deref()
                .unwrap_or_default()
                .contains("No run selected")
        );
        app.input_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.input, "/status");
        app.focus = Focus::Team;
        assert_eq!(app.focus.next(), Focus::Conversation);
        assert_eq!(app.focus.previous(), Focus::Input);
    }

    #[test]
    fn offline_input_never_claims_to_send_a_message() {
        let mut app = Workspace::new(empty());
        app.input = "Build the app".to_owned();
        app.submit();
        assert!(
            app.notice
                .as_deref()
                .unwrap_or_default()
                .contains("not sent or recorded")
        );
        assert!(app.input.is_empty());
    }

    #[test]
    fn palette_and_agent_tabs_have_bounded_navigation() {
        let mut app = Workspace::new(empty());
        app.shift_tab(-1);
        assert_eq!(app.tab, AgentTab::Work);
        app.shift_tab(1);
        assert_eq!(app.tab, AgentTab::Conversation);
        app.palette_index = PaletteAction::ALL
            .iter()
            .position(|item| *item == PaletteAction::Debugger)
            .unwrap();
        assert_eq!(app.execute_palette(), WorkspaceSignal::Debugger);
    }
}
