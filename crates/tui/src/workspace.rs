//! Conversation-first projection of the same run used by Advanced Debugger.
//! Offline input is limited to local inspection commands until a provider
//! and live submission contract are available.

use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use orynth_context::{ContextPrincipal, ContextTrustPolicy, ProjectionRequest};
use orynth_event_store::StoredEvent;
use orynth_kernel::{AgentId, ConflictId, EventKind, FailureId, ModelClass};
use orynth_runtime::RecoveredRun;
use orynth_runtime::conversation::{ConversationSpeaker, ConversationTurn};
use orynth_security::CapabilityDomain;
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
        message_presentation, model_name, tool_name, tool_state_label,
    },
    theme::THEME,
};

const TEXT: Color = THEME.text;
const MUTED: Color = THEME.muted;
const BRAND: Color = THEME.brand;
const ACCENT: Color = THEME.accent;
const WARNING: Color = THEME.warning;
const SURFACE: Color = THEME.surface;
const SELECTED: Color = THEME.surface_selected;
const MAX_CONVERSATION_EVENTS: usize = 128;
const INITIAL_CONVERSATION_EVENTS: usize = 2;
const MAX_OLDER_EVENTS: usize = 512;
const EVENT_PAGE_SIZE: usize = 128;
/// Maximum editable prompt size; clipboard input remains bounded.
const MAX_INPUT_CHARS: usize = 16_384;
const MAX_INPUT_LINES: usize = 6;
const MAX_HISTORY: usize = 32;

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

    fn compact_label(self) -> &'static str {
        match self {
            Self::Conversation => "Chat",
            _ => self.label(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Overlay {
    Help,
    Palette,
    AgentSwitcher,
    Issue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IssueItem {
    Conflict(ConflictId),
    Failure(FailureId),
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
    input_cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    overlay: Option<Overlay>,
    palette_index: usize,
    switcher_index: usize,
    scroll: u16,
    last_max_scroll: Cell<u16>,
    pinned_scroll: bool,
    activity_limit: usize,
    older_events: Vec<StoredEvent>,
    history_before: Option<u64>,
    history_exhausted: bool,
    team_visible: bool,
    last_event_count: u64,
    notice: Option<String>,
    live_text: Option<String>,
}

impl Workspace {
    fn new(snapshot: TuiSnapshot) -> Self {
        let last_event_count = snapshot
            .selected
            .as_ref()
            .map_or(0, |run| run.state.events_applied);
        Self {
            snapshot,
            focus: Focus::Input,
            agent: None,
            selected_team: 0,
            tab: AgentTab::Work,
            input: String::new(),
            input_cursor: 0,
            history: Vec::new(),
            history_index: None,
            overlay: None,
            palette_index: 0,
            switcher_index: 0,
            scroll: 0,
            last_max_scroll: Cell::new(0),
            pinned_scroll: false,
            activity_limit: INITIAL_CONVERSATION_EVENTS,
            older_events: Vec::new(),
            history_before: None,
            history_exhausted: false,
            team_visible: true,
            last_event_count,
            notice: None,
            live_text: None,
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

    fn switcher_workers(&self) -> Vec<AgentId> {
        let Some(run) = self.run() else {
            return Vec::new();
        };
        self.team()
            .into_iter()
            .filter(|id| {
                run.manager
                    .agents
                    .get(id)
                    .is_some_and(|agent| agent.parent_id.is_some())
            })
            .collect()
    }

    fn issue_item(&self, index: usize) -> Option<IssueItem> {
        let run = self.run()?;
        run.assumptions
            .conflicts()
            .keys()
            .copied()
            .map(IssueItem::Conflict)
            .chain(
                run.failures
                    .active_records()
                    .map(|record| IssueItem::Failure(record.id)),
            )
            .nth(index)
    }

    fn selected_issue(&self) -> Option<IssueItem> {
        self.issue_item(self.selected_team.checked_sub(self.team().len())?)
    }

    fn open_selected_agent(&mut self) {
        let Some(id) = self.selected_agent() else {
            self.coordinator();
            return;
        };
        if self
            .run()
            .and_then(|run| run.manager.agents.get(&id))
            .is_some_and(|agent| agent.parent_id.is_none())
        {
            self.coordinator();
            return;
        }
        self.agent = Some(id);
        self.tab = AgentTab::Work;
        self.focus = Focus::Conversation;
        self.scroll = 0;
        self.pinned_scroll = false;
        self.notice = None;
    }

    fn open_switcher(&mut self) {
        let workers = self.switcher_workers();
        self.switcher_index = self
            .agent
            .and_then(|id| workers.iter().position(|candidate| *candidate == id))
            .map_or(0, |index| index + 1);
        self.overlay = Some(Overlay::AgentSwitcher);
    }

    fn choose_switcher_agent(&mut self) {
        let index = self.switcher_index;
        self.overlay = None;
        if index == 0 {
            self.coordinator();
        } else if let Some(id) = self.switcher_workers().get(index - 1).copied() {
            self.selected_team = self
                .team()
                .iter()
                .position(|candidate| *candidate == id)
                .unwrap_or(0);
            self.agent = Some(id);
            self.tab = AgentTab::Work;
            self.focus = Focus::Conversation;
            self.scroll = 0;
            self.pinned_scroll = false;
            self.notice = None;
        }
    }

    fn coordinator(&mut self) {
        self.agent = None;
        self.focus = Focus::Input;
        if self.history_before.is_some() {
            self.older_events.clear();
        }
        self.history_before = None;
        self.history_exhausted = false;
        self.scroll = 0;
        self.pinned_scroll = false;
        self.notice = None;
    }

    fn refresh<S: TuiDataSource>(&mut self, source: &mut S) {
        self.live_text = source.live_text();
        match source.snapshot() {
            Ok(snapshot) => {
                let old_run = self.snapshot.selected.as_ref();
                let next_run = snapshot.selected.as_ref();
                if old_run.map(|run| run.run_id) != next_run.map(|run| run.run_id) {
                    self.older_events.clear();
                    self.history_before = None;
                    self.history_exhausted = false;
                    self.activity_limit = INITIAL_CONVERSATION_EVENTS;
                    self.scroll = 0;
                    self.pinned_scroll = false;
                } else if self.history_before.is_none()
                    && let (Some(old), Some(next)) = (old_run, next_run)
                    && let Some(first) = next.events.first().map(|event| event.sequence)
                {
                    let after = self.older_events.last().map_or(0, |event| event.sequence);
                    self.older_events.extend(
                        old.events
                            .iter()
                            .filter(|event| event.sequence < first && event.sequence > after)
                            .cloned(),
                    );
                    self.older_events.retain(|event| event.sequence < first);
                    let excess = self.older_events.len().saturating_sub(MAX_OLDER_EVENTS);
                    self.older_events.drain(..excess);
                }
                let new_event_count = snapshot
                    .selected
                    .as_ref()
                    .map_or(0, |run| run.state.events_applied);
                if new_event_count > self.last_event_count && self.pinned_scroll {
                    self.notice = Some("New activity below. Press End to follow.".to_owned());
                    if self.agent.is_none() {
                        let added = new_event_count.saturating_sub(self.last_event_count);
                        self.activity_limit = self
                            .activity_limit
                            .saturating_add(added.min(MAX_CONVERSATION_EVENTS as u64) as usize)
                            .min(MAX_CONVERSATION_EVENTS);
                    }
                }
                self.last_event_count = new_event_count;
                self.snapshot = snapshot;
                let entries = self.team().len() + self.issue_count();
                self.selected_team = self.selected_team.min(entries.saturating_sub(1));
                self.switcher_index = self.switcher_index.min(self.switcher_workers().len());
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
                KeyCode::Char('p') => {
                    self.open_switcher();
                    WorkspaceSignal::Stay
                }
                KeyCode::Char('l') => {
                    self.coordinator();
                    WorkspaceSignal::Stay
                }
                KeyCode::Char('b') => {
                    self.team_visible = !self.team_visible;
                    if !self.team_visible && self.focus == Focus::Team {
                        self.focus = Focus::Input;
                    }
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
                Overlay::AgentSwitcher => match key.code {
                    KeyCode::Esc => self.overlay = None,
                    KeyCode::Up => self.switcher_index = self.switcher_index.saturating_sub(1),
                    KeyCode::Down => {
                        self.switcher_index =
                            (self.switcher_index + 1).min(self.switcher_workers().len())
                    }
                    KeyCode::Enter => self.choose_switcher_agent(),
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
            if self.focus == Focus::Team && (!self.team_visible || self.team().is_empty()) {
                self.focus = self.focus.next();
            }
            return Ok(WorkspaceSignal::Stay);
        }
        if key.code == KeyCode::BackTab {
            self.focus = self.focus.previous();
            if self.focus == Focus::Team && (!self.team_visible || self.team().is_empty()) {
                self.focus = self.focus.previous();
            }
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
        if key.code == KeyCode::Char('/') && self.focus != Focus::Input {
            self.focus = Focus::Input;
            self.input_key(key);
            return Ok(WorkspaceSignal::Stay);
        }
        match self.focus {
            Focus::Input => {
                if key.code == KeyCode::Enter && source.is_live() {
                    let value = self.input.trim().to_owned();
                    if !value.is_empty() && !value.starts_with('/') {
                        match source.submit_text(value.clone()) {
                            Ok(()) => {
                                if self.history.len() == MAX_HISTORY {
                                    self.history.remove(0);
                                }
                                self.history.push(value);
                                self.history_index = None;
                                self.input.clear();
                                self.input_cursor = 0;
                                self.notice = Some("Sent to live Coordinator".into());
                                self.refresh(source);
                            }
                            Err(error) => self.notice = Some(error),
                        }
                        return Ok(WorkspaceSignal::Stay);
                    }
                }
                self.input_key(key);
            }
            Focus::Team => match key.code {
                KeyCode::Up => self.selected_team = self.selected_team.saturating_sub(1),
                KeyCode::Down => {
                    let last = (self.team().len() + self.issue_count()).saturating_sub(1);
                    self.selected_team = (self.selected_team + 1).min(last)
                }
                KeyCode::Enter => {
                    if self.selected_issue().is_some() {
                        self.overlay = Some(Overlay::Issue);
                    } else {
                        self.open_selected_agent();
                    }
                }
                KeyCode::Char('!') if self.issue_count() > 0 => {
                    self.selected_team = self.team().len();
                    self.overlay = Some(Overlay::Issue);
                }
                KeyCode::Char('q') => return Ok(WorkspaceSignal::Quit),
                _ => {}
            },
            Focus::Conversation => match key.code {
                KeyCode::Up | KeyCode::PageUp => {
                    let amount = if key.code == KeyCode::PageUp { 8 } else { 1 };
                    if self.agent.is_some() && self.tab != AgentTab::Conversation {
                        self.scroll = self.scroll.saturating_sub(amount);
                    } else {
                        if self.agent.is_none() {
                            if self.activity_limit == MAX_CONVERSATION_EVENTS
                                && self.scroll == 0
                                && !self.history_exhausted
                                && let Some(first) = self.coordination_window().first()
                                && first.sequence > 1
                            {
                                self.history_before = Some(first.sequence);
                                self.scroll = u16::MAX;
                            }
                            self.activity_limit = (self.activity_limit + amount as usize)
                                .min(MAX_CONVERSATION_EVENTS);
                            if let Err(error) = self.load_older_if_needed(source) {
                                self.notice =
                                    Some(format!("Could not load older activity: {error}"));
                            }
                        }
                        self.scroll = self
                            .conversation_offset(self.last_max_scroll.get())
                            .saturating_sub(amount);
                        self.pinned_scroll = true;
                    }
                }
                KeyCode::Down | KeyCode::PageDown => {
                    let amount = if key.code == KeyCode::PageDown { 8 } else { 1 };
                    if self.agent.is_some() && self.tab != AgentTab::Conversation {
                        self.scroll = self.scroll.saturating_add(amount);
                    } else {
                        self.scroll = self
                            .conversation_offset(self.last_max_scroll.get())
                            .saturating_add(amount);
                        if self.scroll >= self.last_max_scroll.get() {
                            self.scroll = 0;
                            self.pinned_scroll = false;
                            self.notice = None;
                        }
                    }
                }
                KeyCode::End => {
                    if self.history_before.is_some() {
                        self.older_events.clear();
                    }
                    self.history_before = None;
                    self.history_exhausted = false;
                    self.scroll = 0;
                    self.pinned_scroll = false;
                    self.notice = None;
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

    fn conversation_offset(&self, max_scroll: u16) -> u16 {
        if self.pinned_scroll {
            self.scroll.min(max_scroll)
        } else {
            max_scroll
        }
    }

    fn load_older_if_needed<S: TuiDataSource>(&mut self, source: &mut S) -> Result<(), String> {
        if self.history_exhausted {
            return Ok(());
        }
        let Some(run) = self.run() else {
            return Ok(());
        };
        let available = run
            .events
            .iter()
            .chain(self.older_events.iter())
            .filter(|event| {
                self.history_before
                    .is_none_or(|before| event.sequence < before)
                    && is_coordination_event(&event.event.kind)
            })
            .count();
        if available >= self.activity_limit {
            return Ok(());
        }
        let before = self
            .older_events
            .first()
            .or_else(|| run.events.first())
            .map(|event| event.sequence);
        let Some(before) = before else {
            self.history_exhausted = true;
            return Ok(());
        };
        let page = source.event_page(Some(before), EVENT_PAGE_SIZE)?;
        if page.is_empty() {
            self.history_exhausted = true;
            return Ok(());
        }
        let mut page = page
            .into_iter()
            .filter(|event| event.sequence < before)
            .collect::<Vec<_>>();
        if page.is_empty() {
            self.history_exhausted = true;
        } else {
            page.append(&mut self.older_events);
            page.truncate(MAX_OLDER_EVENTS);
            self.older_events = page;
        }
        Ok(())
    }

    fn coordination_window(&self) -> Vec<&StoredEvent> {
        let Some(run) = self.run() else {
            return Vec::new();
        };
        run.events
            .iter()
            .rev()
            .chain(self.older_events.iter().rev())
            .filter(|stored| {
                self.history_before
                    .is_none_or(|before| stored.sequence < before)
                    && is_coordination_event(&stored.event.kind)
            })
            .take(self.activity_limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    fn input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    let end = char_boundary(&self.input, self.input_cursor);
                    let start = char_boundary(&self.input, self.input_cursor - 1);
                    self.input.replace_range(start..end, "");
                    self.input_cursor -= 1;
                }
                self.history_index = None;
            }
            KeyCode::Delete => {
                if self.input_cursor < self.input.chars().count() {
                    let start = char_boundary(&self.input, self.input_cursor);
                    let end = char_boundary(&self.input, self.input_cursor + 1);
                    self.input.replace_range(start..end, "");
                }
                self.history_index = None;
            }
            KeyCode::Left => self.input_cursor = self.input_cursor.saturating_sub(1),
            KeyCode::Right => {
                self.input_cursor = (self.input_cursor + 1).min(self.input.chars().count())
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input.chars().count(),
            KeyCode::Up => self.history_step(-1),
            KeyCode::Down => self.history_step(1),
            KeyCode::Char(c) if self.input.chars().count() < MAX_INPUT_CHARS => {
                self.input
                    .insert(char_boundary(&self.input, self.input_cursor), c);
                self.input_cursor += 1;
                self.history_index = None;
            }
            _ => {}
        }
    }

    /// Insert a bracketed-paste payload at the cursor. Newlines are data, not
    /// synthetic Enter keys, so paste can never submit a prompt.
    fn paste(&mut self, pasted: &str) {
        let sanitized = sanitize_paste(pasted);
        if sanitized.is_empty() {
            return;
        }
        let length = self.input.chars().count();
        let paste_length = sanitized.chars().count();
        if length
            .checked_add(paste_length)
            .is_none_or(|length| length > MAX_INPUT_CHARS)
        {
            self.notice = Some(format!(
                "Paste is too large for the current input limit ({} characters).",
                MAX_INPUT_CHARS
            ));
            return;
        }
        let byte_cursor = char_boundary(&self.input, self.input_cursor);
        self.input.insert_str(byte_cursor, &sanitized);
        self.input_cursor += paste_length;
        self.history_index = None;
        self.notice = None;
    }

    fn history_step(&mut self, direction: isize) {
        if self.history.is_empty() {
            return;
        }
        let index = self.history_index.unwrap_or(self.history.len());
        let next = (index as isize + direction).clamp(0, self.history.len() as isize) as usize;
        self.history_index = Some(next);
        self.input = self.history.get(next).cloned().unwrap_or_default();
        self.input_cursor = self.input.chars().count();
    }

    fn open_agent_named(&mut self, name: &str) -> bool {
        let Some((index, id)) = self.team().into_iter().enumerate().find(|(_, id)| {
            self.run()
                .and_then(|run| run.manager.agents.get(id))
                .is_some_and(|agent| {
                    agent.parent_id.is_some() && agent.name.eq_ignore_ascii_case(name)
                })
        }) else {
            return false;
        };
        self.selected_team = index;
        self.agent = Some(id);
        self.tab = AgentTab::Work;
        self.focus = Focus::Conversation;
        self.scroll = 0;
        self.pinned_scroll = false;
        self.notice = None;
        true
    }

    fn submit(&mut self) {
        let value = self.input.trim().to_owned();
        if value.is_empty() {
            return;
        }
        if self.history.len() == MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(value.clone());
        self.history_index = None;
        self.input.clear();
        self.input_cursor = 0;
        let mut parts = value.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "/help" => self.overlay = Some(Overlay::Help),
            "/coordinator" => self.coordinator(),
            "/agents" => {
                self.team_visible = true;
                self.focus = Focus::Team;
            }
            "/status" => self.notice = Some(self.status_text()),
            "/conflicts" => {
                if self.issue_count() > 0 {
                    self.selected_team = self.team().len();
                    self.overlay = Some(Overlay::Issue);
                }
                else { self.notice = Some("No current issues in this run.".to_owned()); }
            }
            "/agent" => {
                let name = parts.next().unwrap_or_default();
                if name.is_empty() || parts.next().is_some() {
                    self.notice = Some("Use /agent NAME, or press Ctrl+P to choose an agent.".to_owned());
                } else if !self.open_agent_named(name) {
                    self.notice = Some(format!("No agent named {name}. Press Ctrl+P to choose."));
                } else {
                    self.notice = None;
                }
            }
            "/tools" => { self.open_selected_agent(); self.tab = AgentTab::Tools; }
            "/context" => { self.open_selected_agent(); self.tab = AgentTab::Context; }
            "/runs" => self.notice = Some("Opening run history...".to_owned()),
            "/debug" => self.notice = Some("Opening Advanced Debugger...".to_owned()),
            _ if command.starts_with('/') => self.notice = Some(format!("Unknown command {command}. Use /help.")),
            _ if self.snapshot.presentation.as_ref().is_some_and(|view| view.demo) => {
                self.notice = Some("OFFLINE DEMO / MOCK MODE: mock agents cannot execute new AI tasks. Connect a model provider to run real work. Your message was not sent or recorded.".to_owned());
            }
            _ => self.notice = Some("Offline workspace: no Coordinator model is connected. Your message was not sent or recorded.".to_owned()),
        }
    }

    fn execute_palette(&mut self) -> WorkspaceSignal {
        self.overlay = None;
        match PaletteAction::ALL[self.palette_index] {
            PaletteAction::Coordinator => self.coordinator(),
            PaletteAction::OpenSelectedAgent => self.open_selected_agent(),
            PaletteAction::Team => {
                self.team_visible = true;
                self.focus = Focus::Team;
            }
            PaletteAction::Issues => {
                if self.issue_count() > 0 {
                    self.selected_team = self.team().len();
                    self.overlay = Some(Overlay::Issue)
                } else {
                    self.notice = Some("No current issues.".to_owned())
                }
            }
            PaletteAction::ToolActivity => {
                self.open_selected_agent();
                self.tab = AgentTab::Tools;
            }
            PaletteAction::RunHistory => return WorkspaceSignal::DebuggerRuns,
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
        let input_height = input_height(&self.input, area.width);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(input_height),
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(if self.notice.is_some() { 3 } else { 1 }),
            ])
            .split(area);
        self.render_header(frame, rows[0]);
        if self.team_visible && self.run().is_some() && area.width >= 72 {
            let sidebar_width = if area.width < 104 {
                28
            } else {
                (area.width / 4).max(28)
            };
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(sidebar_width)])
                .split(rows[1]);
            self.render_main(frame, cols[0]);
            self.render_team(frame, cols[1]);
        } else if self.team_visible && self.run().is_some() && self.focus == Focus::Team {
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
        let mut lines = vec![Line::from(vec![
            Span::styled(
                " ORYNTH  ",
                Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
            ),
            Span::styled(title.to_owned(), Style::default().fg(TEXT)),
            Span::styled(format!("  /  {summary}"), Style::default().fg(MUTED)),
        ])];
        if let Some(view) = self
            .snapshot
            .presentation
            .as_ref()
            .filter(|view| view.title.starts_with("LIVE"))
        {
            lines.push(Line::styled(
                format!(" {}", view.description),
                Style::default().fg(MUTED),
            ));
        }
        if self
            .snapshot
            .presentation
            .as_ref()
            .is_some_and(|view| view.demo)
        {
            lines.push(Line::styled(
                " OFFLINE DEMO / MOCK MODE  ·  No real AI tasks",
                Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
            ));
        }
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
                            format!(
                                "  {}  ",
                                if area.width < 72 {
                                    tab.compact_label()
                                } else {
                                    tab.label()
                                }
                            ),
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
        let wrapped_lines = lines
            .iter()
            .map(|line| line.width().max(1).div_ceil(body.width.max(1) as usize))
            .sum::<usize>();
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let visible = body.height as usize;
        let max_scroll = wrapped_lines.saturating_sub(visible).min(u16::MAX as usize) as u16;
        self.last_max_scroll.set(max_scroll);
        let offset = if self.agent.is_some() && self.tab != AgentTab::Conversation {
            self.scroll.min(max_scroll)
        } else {
            self.conversation_offset(max_scroll)
        };
        frame.render_widget(paragraph.scroll((offset, 0)), body);
    }

    fn coordinator_lines(&self) -> Vec<Line<'static>> {
        let Some(run) = self.run() else {
            if self
                .snapshot
                .presentation
                .as_ref()
                .is_some_and(|view| view.title.starts_with("LIVE"))
            {
                return vec![
                    heading("LIVE · OPENROUTER"),
                    Line::raw(""),
                    Line::raw("Type a message to the Coordinator and press Enter."),
                ];
            }
            return vec![
                Line::styled(
                    "NO ACTIVE RUN",
                    Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                Line::raw(
                    "Orynth lets you work with one Coordinator while specialist agents work in the background.",
                ),
                Line::raw(""),
                Line::raw("Try the recorded offline demo: orynth --demo"),
                Line::raw(""),
                Line::raw("Coordinator messages cannot be sent until a provider is connected."),
            ];
        };
        let mut lines = vec![Line::styled(
            if self
                .snapshot
                .presentation
                .as_ref()
                .is_some_and(|view| view.title.starts_with("LIVE"))
            {
                "COORDINATOR · LIVE · OPENROUTER"
            } else {
                "COORDINATOR · RECORDED RUN"
            },
            Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
        )];
        if let Some(task) = run.state.tasks.values().next() {
            lines.push(Line::raw(format!("  Goal: {}", task.title)));
        }
        lines.push(Line::raw(""));
        let team = self.team();
        if team.len() > 1 {
            let names = team
                .iter()
                .skip(1)
                .take(5)
                .filter_map(|id| run.manager.agents.get(id).map(|agent| agent.name.as_str()))
                .collect::<Vec<_>>()
                .join(" · ");
            lines.push(Line::raw(format!("  Team: {names}")));
            lines.push(Line::raw(""));
        }
        if let Some(conflict) = run.assumptions.conflicts().values().next() {
            lines.push(Line::styled(
                format!("  ! Conflict: {} · inspect in AI Team", conflict.subject),
                Style::default().fg(WARNING),
            ));
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            "RECORDED ACTIVITY",
            Style::default().fg(ACCENT),
        ));
        for stored in self.coordination_window() {
            if let EventKind::ConversationTurn { version, payload } = &stored.event.kind
                && let Ok(turn) = ConversationTurn::decode(*version, payload)
            {
                let speaker = match turn.speaker {
                    ConversationSpeaker::User => "You",
                    ConversationSpeaker::Coordinator(_) => "Coordinator",
                };
                lines.push(heading(speaker));
                lines.extend(conversation_content_lines(&turn.content));
                lines.push(Line::raw(""));
                continue;
            }
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
        if let Some(text) = &self.live_text {
            lines.push(heading("Coordinator · streaming"));
            lines.extend(conversation_content_lines(text));
            lines.push(Line::raw(""));
        }
        if self.pinned_scroll {
            lines.push(Line::styled(
                if self.notice.as_deref() == Some("New activity below. Press End to follow.") {
                    "↓ New activity below · End to follow"
                } else if self.history_before.is_some() {
                    "↑ Older activity · End to follow live"
                } else {
                    "↓ End to follow current activity"
                },
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
                    heading("RECENT ACTIVITY"),
                    Line::raw(
                        run.messages
                            .iter()
                            .rev()
                            .find(|message| message.sender == id || message.recipient == id)
                            .map_or_else(
                                || "No recorded agent exchange yet.".to_owned(),
                                |message| {
                                    let view = message_presentation(run, message);
                                    format!("{} · {}", view.kind, view.title)
                                },
                            ),
                    ),
                    Line::raw(""),
                    heading("MODEL"),
                    Line::raw(format!(
                        "{} · {}",
                        model_name(&agent.model),
                        model_tier(&agent.model.class)
                    )),
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
                if let Some(failure) = agent
                    .active_failure_ids
                    .iter()
                    .find_map(|id| run.failures.records().get(id))
                {
                    lines.push(Line::styled(
                        format!("Blocked: {}", failure.reason),
                        Style::default().fg(WARNING),
                    ));
                }
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
                    lines.extend(conversation_content_lines(&view.body));
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
                    lines.push(Line::raw(format!(
                        "Result: {}",
                        tool_state_label(record.state)
                    )));
                    lines.push(Line::raw(""));
                }
                if lines.is_empty() {
                    lines.push(Line::raw("No recorded tool activity."));
                }
            }
            AgentTab::Access => {
                lines.push(heading("RUNTIME CAPABILITY LEASES"));
                let leases = run
                    .capabilities
                    .leases()
                    .values()
                    .filter(|lease| lease.agent_id == id)
                    .take(32)
                    .collect::<Vec<_>>();
                if leases.is_empty() {
                    lines.push(Line::raw("No capability leases recorded."));
                }
                for lease in &leases {
                    lines.push(Line::raw(format!(
                        "{} · {}{}",
                        capability_domain_label(lease.domain),
                        lease.resource,
                        if lease.task_id.is_some() {
                            " · task scoped"
                        } else {
                            ""
                        }
                    )));
                }
                lines.push(Line::raw(""));
                lines.push(heading("NETWORK"));
                if leases
                    .iter()
                    .any(|lease| lease.domain == CapabilityDomain::Network)
                {
                    lines.push(Line::raw("See network leases above."));
                } else {
                    lines.push(Line::raw("No network lease recorded."));
                }
                lines.push(Line::raw(""));
                lines.push(heading("OWNED FOR CHANGES"));
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
                lines.push(Line::styled(
                    "Every action still passes runtime capability and ownership checks.",
                    Style::default().fg(MUTED),
                ));
            }
        }
        lines
    }

    fn render_team(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = Vec::new();
        let compact = area.width < 30;
        if let Some(run) = self.run() {
            let team = self.team();
            let rows_per_agent = if compact { 3 } else { 5 };
            let reserved = (if self.issue_count() > 0 { 4 } else { 2 })
                + usize::from(self.focus == Focus::Team);
            let visible_agents =
                ((area.height as usize).saturating_sub(reserved) / rows_per_agent).max(1);
            let start = self
                .selected_team
                .min(team.len().saturating_sub(1))
                .saturating_sub(visible_agents.saturating_sub(1));
            if self.focus == Focus::Team {
                lines.push(Line::styled(
                    "  ↑↓ MOVE  ENTER OPEN",
                    Style::default().fg(BRAND).add_modifier(Modifier::BOLD),
                ));
            }
            if start > 0 {
                lines.push(Line::styled(
                    format!("  ↑ {start} more agents"),
                    Style::default().fg(MUTED),
                ));
            }
            for (index, id) in team.iter().enumerate().skip(start).take(visible_agents) {
                let agent = &run.manager.agents[id];
                let selected = index == self.selected_team;
                let marker = if selected { "›" } else { " " };
                let focused_selected = selected && self.focus == Focus::Team;
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
                        .fg(if focused_selected {
                            Color::Black
                        } else if selected {
                            BRAND
                        } else {
                            TEXT
                        })
                        .bg(if focused_selected { BRAND } else { SURFACE })
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
                    Style::default()
                        .fg(if focused_selected { TEXT } else { MUTED })
                        .bg(if focused_selected { SELECTED } else { SURFACE }),
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
                    let model = model_name(&agent.model);
                    let short_model = model.split_whitespace().next().unwrap_or("Model");
                    lines.push(Line::styled(
                        format!("  {marker} {status} · {short_model}"),
                        Style::default()
                            .bg(if focused_selected { SELECTED } else { SURFACE })
                            .fg(if health == "Needs attention" {
                                WARNING
                            } else {
                                MUTED
                            }),
                    ));
                } else {
                    lines.push(Line::styled(
                        format!("  {status} · {health}"),
                        Style::default()
                            .bg(if focused_selected { SELECTED } else { SURFACE })
                            .fg(if health == "Needs attention" {
                                WARNING
                            } else {
                                MUTED
                            }),
                    ));
                    lines.push(Line::styled(
                        format!("  Model: {}", model_name(&agent.model)),
                        Style::default().fg(MUTED).bg(if focused_selected {
                            SELECTED
                        } else {
                            SURFACE
                        }),
                    ));
                    lines.push(Line::raw(""));
                }
            }
            let remaining = team.len().saturating_sub(start + visible_agents);
            if remaining > 0 {
                lines.push(Line::styled(
                    format!("  ↓ {remaining} more agents"),
                    Style::default().fg(MUTED),
                ));
            }
            if self.issue_count() > 0 {
                let selected = self.selected_team >= team.len();
                let index = if selected {
                    self.selected_team - team.len()
                } else {
                    0
                };
                if let Some(issue) = self.issue_item(index) {
                    lines.push(heading(&format!(
                        "ATTENTION · {} / {}",
                        index + 1,
                        self.issue_count()
                    )));
                    let label = match issue {
                        IssueItem::Conflict(id) => {
                            run.assumptions.conflicts().get(&id).map_or_else(
                                || "Project conflict".to_owned(),
                                |item| format!("{} conflict", item.subject),
                            )
                        }
                        IssueItem::Failure(id) => run.failures.records().get(&id).map_or_else(
                            || "Recorded failure".to_owned(),
                            |item| format!("{} failed", agent_label(run, item.agent_id)),
                        ),
                    };
                    lines.push(Line::styled(
                        format!("{} ! {label}", if selected { ">" } else { " " }),
                        Style::default()
                            .fg(if selected && self.focus == Focus::Team {
                                Color::Black
                            } else if selected {
                                BRAND
                            } else {
                                WARNING
                            })
                            .bg(if selected && self.focus == Focus::Team {
                                BRAND
                            } else {
                                SURFACE
                            }),
                    ));
                    if !compact {
                        lines.push(Line::styled(
                            "  Enter inspects issue",
                            Style::default().fg(MUTED),
                        ));
                    }
                }
            }
        } else {
            lines.push(Line::raw("No team selected."));
        }
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(if self.focus == Focus::Team {
                        Borders::ALL
                    } else {
                        Borders::LEFT
                    })
                    .title(if self.focus == Focus::Team {
                        " AI TEAM · FOCUSED "
                    } else {
                        " AI TEAM "
                    })
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
            let placeholder = self
                .agent
                .and_then(|id| self.run().and_then(|run| run.manager.agents.get(&id)))
                .map_or_else(
                    || "What would you like Orynth to do?".to_owned(),
                    |agent| format!("Ask the Coordinator about {}...", agent.name),
                );
            if self.focus == Focus::Input {
                format!("▏{placeholder}")
            } else {
                placeholder
            }
        } else {
            let cursor = self.input_cursor.min(self.input.chars().count());
            let mut visible = self.input.clone();
            if self.focus == Focus::Input {
                visible.insert(char_boundary(&visible, cursor), '▏');
            }
            visible
        };
        let title = self
            .agent
            .and_then(|id| self.run().and_then(|run| run.manager.agents.get(&id)))
            .map_or_else(
                || " COORDINATOR ".to_owned(),
                |agent| format!(" Viewing {} · talk to Coordinator below ", agent.name),
            );
        frame.render_widget(
            Paragraph::new(content)
                .wrap(Wrap { trim: false })
                .scroll((input_scroll(&self.input, self.input_cursor, area.width), 0))
                .block(
                    Block::default()
                        .borders(Borders::TOP | Borders::BOTTOM)
                        .title(title)
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
        if let Some(notice) = &self.notice {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(area);
            frame.render_widget(
                Paragraph::new(notice.as_str())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(WARNING)),
                rows[0],
            );
            frame.render_widget(
                Paragraph::new(self.footer_text()).style(Style::default().fg(MUTED)),
                rows[1],
            );
        } else {
            frame.render_widget(
                Paragraph::new(self.footer_text()).style(Style::default().fg(MUTED)),
                area,
            );
        }
    }

    fn footer_text(&self) -> &'static str {
        match self.overlay {
            Some(Overlay::AgentSwitcher) => "↑↓ Select   Enter Open   Esc Close",
            Some(Overlay::Palette) => "↑↓ Select   Enter Run   Esc Close",
            Some(Overlay::Help | Overlay::Issue) => "Enter/Esc Close   Ctrl+C Quit",
            None if !self.team_visible && self.agent.is_some() => {
                "Ctrl+B Show team   Ctrl+P Switch Agent   Esc Coordinator"
            }
            None if !self.team_visible => "Ctrl+B Show team   Ctrl+P Agents   Ctrl+K Commands",
            None if self.focus == Focus::Team => {
                "↑↓ Select   Enter Open   Esc Coordinator   Ctrl+P Agents"
            }
            None if self.agent.is_some() && self.focus == Focus::Conversation => {
                "Tab Input   ←→ Sections   Esc Coordinator   Ctrl+P Switch"
            }
            None if self.agent.is_some() => {
                "Tab Focus team   Esc Coordinator   Ctrl+P Switch Agent"
            }
            None if self.team().is_empty() => "Ctrl+K Commands   ? Help   Try orynth --demo",
            None if !self.input.is_empty() && self.focus == Focus::Input => {
                "Enter Submit   Tab Focus team   Ctrl+P Agents   Ctrl+K Commands"
            }
            None if self.focus == Focus::Conversation => {
                "Tab Input   ↑↓ Scroll   Ctrl+P Agents   Esc Coordinator"
            }
            None => "Tab Focus team   Ctrl+P Agents   Ctrl+K Commands   ? Help",
        }
    }

    fn render_overlay(&self, frame: &mut Frame<'_>, area: Rect, overlay: Overlay) {
        let width = area.width.saturating_sub(4).clamp(1, 72);
        let height = if overlay == Overlay::AgentSwitcher {
            (self.switcher_workers().len() as u16 + 4)
                .min(area.height.saturating_sub(2))
                .clamp(1, 18)
        } else {
            area.height.saturating_sub(2).clamp(1, 22)
        };
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
                    Line::raw("Tab focuses AI Team. The bright row is the selection."),
                    Line::raw("Up/Down selects; Enter opens; Esc returns to Coordinator."),
                    Line::raw("Ctrl+P opens the agent switcher from anywhere."),
                    Line::raw("Workers are read-only. Input still talks to Coordinator."),
                    Line::raw("Left/Right changes worker sections. Ctrl+K opens commands."),
                    Line::raw("/agent NAME and /coordinator also switch views."),
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
            Overlay::AgentSwitcher => {
                let workers = self.switcher_workers();
                let visible = height.saturating_sub(2).max(1) as usize;
                let selected = self.switcher_index.min(workers.len());
                let start = selected.saturating_sub(visible.saturating_sub(1));
                let mut lines = Vec::new();
                for index in start..=workers.len().min(start + visible - 1) {
                    let label = if index == 0 {
                        "COORDINATOR".to_owned()
                    } else if let Some(agent) = workers
                        .get(index - 1)
                        .and_then(|id| self.run()?.manager.agents.get(id))
                    {
                        let (status, _) = agent_status(agent.status, agent.health.status);
                        format!("{}  ·  {}  ·  {status}", agent.name, agent_role(agent))
                    } else {
                        continue;
                    };
                    let is_selected = index == selected;
                    lines.push(Line::styled(
                        format!("{} {label}", if is_selected { "▶" } else { " " }),
                        Style::default()
                            .fg(if is_selected { Color::Black } else { TEXT })
                            .bg(if is_selected { BRAND } else { SURFACE })
                            .add_modifier(if is_selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ));
                }
                ("SWITCH AGENT · ↑↓ Select · Enter Open · Esc Close", lines)
            }
            Overlay::Issue => ("NEEDS ATTENTION", self.issue_lines()),
        };
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} "))
                .border_style(Style::default().fg(BRAND))
                .style(Style::default().bg(SURFACE)),
        );
        let paragraph = if overlay == Overlay::AgentSwitcher {
            paragraph
        } else {
            paragraph.wrap(Wrap { trim: false })
        };
        frame.render_widget(paragraph, rect);
    }

    fn issue_lines(&self) -> Vec<Line<'static>> {
        let Some(run) = self.run() else {
            return vec![Line::raw("No selected run.")];
        };
        let issue = self.selected_issue().or_else(|| self.issue_item(0));
        if let Some(IssueItem::Conflict(id)) = issue
            && let Some(conflict) = run.assumptions.conflicts().get(&id)
        {
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
            return lines;
        }
        if let Some(IssueItem::Failure(id)) = issue
            && let Some(failure) = run.failures.records().get(&id)
        {
            return vec![
                heading(&format!(
                    "{} needs attention",
                    agent_label(run, failure.agent_id)
                )),
                Line::raw(""),
                Line::raw(format!("Approach: {}", failure.approach)),
                Line::raw(format!("Reason: {}", failure.reason)),
                Line::raw(""),
                Line::raw("This failed approach is recorded in runtime memory. Esc closes."),
            ];
        }
        vec![Line::raw("No current issues.")]
    }
}

fn heading(value: &str) -> Line<'static> {
    Line::styled(
        value.to_owned(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )
}

fn conversation_content_lines(content: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in content.lines() {
        if let Some(language) = raw.trim_start().strip_prefix("```") {
            let label = if in_code {
                "  └─".to_owned()
            } else if language.trim().is_empty() {
                "  ┌─ code".to_owned()
            } else {
                format!("  ┌─ {}", language.trim())
            };
            lines.push(Line::styled(label, Style::default().fg(ACCENT).bg(SURFACE)));
            in_code = !in_code;
        } else if in_code {
            lines.push(Line::styled(
                format!("  │ {raw}"),
                Style::default().fg(TEXT).bg(SURFACE),
            ));
        } else {
            lines.push(Line::raw(format!("  {raw}")));
        }
    }
    lines
}

fn is_coordination_event(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::AgentCreated { .. }
            | EventKind::AgentPaused { .. }
            | EventKind::AgentResumed { .. }
            | EventKind::AssumptionTransition { .. }
            | EventKind::ToolTransition { .. }
            | EventKind::ModelRequested { .. }
            | EventKind::ModelFailed { .. }
            | EventKind::RunCompleted { .. }
            | EventKind::RunFailed { .. }
            | EventKind::ConversationTurn { .. }
    )
}

fn capability_domain_label(domain: CapabilityDomain) -> &'static str {
    match domain {
        CapabilityDomain::Filesystem => "Files",
        CapabilityDomain::Process => "Process",
        CapabilityDomain::Network => "Network",
        CapabilityDomain::Secrets => "Secrets",
        CapabilityDomain::Plugins => "Plugins",
        CapabilityDomain::ExternalServices => "External service",
    }
}

fn char_boundary(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map_or(value.len(), |(index, _)| index)
}

fn sanitize_paste(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\r' => {}
            '\n' | '\t' => sanitized.push(character),
            character if !character.is_control() => sanitized.push(character),
            _ => {}
        }
    }
    sanitized
}

fn wrapped_input_lines(value: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    value
        .split('\n')
        .map(|line| line.chars().count().div_ceil(width).max(1))
        .sum::<usize>()
        .max(1)
}

fn input_height(value: &str, width: u16) -> u16 {
    (wrapped_input_lines(value, width.saturating_sub(2)) + 2)
        .clamp(3, MAX_INPUT_LINES) as u16
}

fn input_scroll(value: &str, cursor: usize, width: u16) -> u16 {
    let byte_cursor = char_boundary(value, cursor.min(value.chars().count()));
    let before = &value[..byte_cursor];
    let content_width = usize::from(width.saturating_sub(2).max(1));
    let cursor_line = before
        .split('\n')
        .map(|line| line.chars().count().div_ceil(content_width))
        .sum::<usize>()
        .saturating_sub(1);
    let visible = usize::from(input_height(value, width).saturating_sub(2));
    cursor_line
        .saturating_sub(visible.saturating_sub(1))
        .min(u16::MAX as usize) as u16
}

fn model_tier(class: &ModelClass) -> &'static str {
    match class {
        ModelClass::Cheap => "Economy",
        ModelClass::Strong => "Strong",
        ModelClass::Local => "Local",
        ModelClass::Custom(_) => "Custom",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkspaceSignal {
    Stay,
    Debugger,
    DebuggerRuns,
    Quit,
}

pub fn run_workspace<S: TuiDataSource>(source: S) -> Result<(), String> {
    run_workspace_started_at(source, Instant::now())
}

/// Launch with a caller timestamp so the first-frame benchmark includes
/// offline demo construction and run-source setup.
pub fn run_workspace_started_at<S: TuiDataSource>(
    mut source: S,
    started_at: Instant,
) -> Result<(), String> {
    let snapshot = source.snapshot()?;
    let mut workspace = Workspace::new(snapshot.clone());
    let mut debugger = TuiApp::embedded(snapshot);
    let mut debug_mode = false;
    let mut last_refresh = Instant::now();
    let mut first_frame_recorded = false;
    let _guard =
        TerminalGuard::enter().map_err(|error| format!("could not enter terminal UI: {error}"))?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|error| format!("could not create terminal UI: {error}"))?;
    loop {
        if !debug_mode && source.is_live() && last_refresh.elapsed() >= Duration::from_millis(200) {
            workspace.refresh(&mut source);
            last_refresh = Instant::now();
        }
        terminal
            .draw(|frame| {
                if debug_mode {
                    debugger.render(frame)
                } else {
                    workspace.render(frame)
                }
            })
            .map_err(|error| format!("could not render terminal UI: {error}"))?;
        if !first_frame_recorded {
            if let Some(path) = std::env::var_os("ORYNTH_TUI_FIRST_RENDER_US_PATH") {
                let _ = std::fs::write(path, started_at.elapsed().as_micros().to_string());
            }
            first_frame_recorded = true;
        }
        if !event::poll(Duration::from_millis(250))
            .map_err(|error| format!("terminal input failed: {error}"))?
        {
            if !debug_mode && !source.is_live() && last_refresh.elapsed() >= Duration::from_secs(2)
            {
                workspace.refresh(&mut source);
                last_refresh = Instant::now();
            }
            continue;
        }
        let terminal_event =
            event::read().map_err(|error| format!("terminal input failed: {error}"))?;
        if let Event::Paste(text) = terminal_event {
            if !debug_mode && workspace.overlay.is_none() && workspace.focus == Focus::Input {
                workspace.paste(&text);
            }
            continue;
        }
        let Event::Key(key) = terminal_event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
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
                && matches!(workspace.input.trim(), "/debug" | "/runs")
            {
                let runs = workspace.input.trim() == "/runs";
                workspace.input.clear();
                workspace.input_cursor = 0;
                debug_mode = true;
                debugger = TuiApp::embedded(source.snapshot()?);
                if runs {
                    debugger.open_runs();
                }
                continue;
            }
            match workspace.key(key, &mut source)? {
                WorkspaceSignal::Stay => {}
                WorkspaceSignal::Debugger => {
                    debug_mode = true;
                    debugger = TuiApp::embedded(source.snapshot()?);
                }
                WorkspaceSignal::DebuggerRuns => {
                    debug_mode = true;
                    debugger = TuiApp::embedded(source.snapshot()?);
                    debugger.open_runs();
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
    use orynth_assumptions::AssumptionGraph;
    use orynth_cache::CacheTelemetry;
    use orynth_context::{ContextGraph, ContextProprioception};
    use orynth_event_store::{AgentStatus, RunStatus, RuntimeState};
    use orynth_failure_memory::{FailureMemory, FailureRecord, FailureTransition};
    use orynth_kernel::{Event, ModelRef, RunId};
    use orynth_runtime::{ManagerAgentProjection, ManagerProjection};
    use orynth_scheduler::SchedulerState;
    use orynth_security::CapabilityPolicy;
    use orynth_specialist::SpecialistRegistry;
    use orynth_tool_runtime::ToolHistory;

    struct EmptySource;

    impl TuiDataSource for EmptySource {
        fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
            Ok(empty())
        }
        fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
            Ok(())
        }
    }

    struct LiveSource {
        submitted: Vec<String>,
    }

    impl TuiDataSource for LiveSource {
        fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
            Ok(empty())
        }

        fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
            Ok(())
        }

        fn submit_text(&mut self, text: String) -> Result<(), String> {
            self.submitted.push(text);
            Ok(())
        }

        fn is_live(&self) -> bool {
            true
        }
    }

    fn empty() -> TuiSnapshot {
        TuiSnapshot {
            selected: None,
            runs: Vec::new(),
            presentation: None,
        }
    }

    fn run_with_events(events: Vec<StoredEvent>) -> RecoveredRun {
        let run_id = RunId::from_u64(1);
        RecoveredRun {
            run_id,
            state: RuntimeState {
                run_id,
                status: RunStatus::Active,
                tasks: Default::default(),
                agents: Default::default(),
                artifacts: Default::default(),
                events_applied: events.len() as u64,
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
        }
    }

    fn navigation_snapshot() -> TuiSnapshot {
        let mut run = run_with_events(Vec::new());
        let root = AgentId::from_u64(10);
        for (number, name, parent, status) in [
            (10, "COORDINATOR", None, AgentStatus::Running),
            (11, "AUTH-01", Some(root), AgentStatus::Running),
            (12, "DB-02", Some(root), AgentStatus::Running),
            (13, "SEC-03", Some(root), AgentStatus::Paused),
        ] {
            let id = AgentId::from_u64(number);
            run.manager.agents.insert(
                id,
                ManagerAgentProjection {
                    agent_id: id,
                    name: name.to_owned(),
                    mission: format!("{name} mission"),
                    model: ModelRef::new("demo", "mock-model", ModelClass::Cheap),
                    status,
                    chunks_received: 0,
                    usage: Default::default(),
                    parent_id: parent,
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
        }
        TuiSnapshot {
            selected: Some(run),
            runs: Vec::new(),
            presentation: Some(crate::fullscreen::TuiPresentation {
                title: "Navigation demo".to_owned(),
                description: "Offline team".to_owned(),
                demo: true,
            }),
        }
    }

    fn rendered_workspace(app: &Workspace, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| app.render(frame))
            .expect("render workspace");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |cell| cell.symbol()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn team_focus_arrows_enter_and_escape_are_visible_and_work() {
        let mut app = Workspace::new(navigation_snapshot());
        let mut source = EmptySource;
        assert!(app.footer_text().contains("Tab Focus team"));
        assert!(rendered_workspace(&app, 100, 30).contains("OFFLINE DEMO / MOCK MODE"));
        app.key(KeyEvent::from(KeyCode::Tab), &mut source).unwrap();
        assert_eq!(app.focus, Focus::Team);
        assert!(app.footer_text().contains("↑↓ Select   Enter Open"));
        let focused = rendered_workspace(&app, 100, 30);
        assert!(focused.contains("AI TEAM · FOCUSED"));
        assert!(focused.contains("↑↓ MOVE  ENTER OPEN"));
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!((3..27).any(|y| {
            (72..100).any(|x| {
                terminal
                    .backend()
                    .buffer()
                    .cell((x, y))
                    .is_some_and(|cell| cell.bg == BRAND && cell.fg == Color::Black)
            })
        }));
        app.key(KeyEvent::from(KeyCode::Down), &mut source).unwrap();
        app.key(KeyEvent::from(KeyCode::Down), &mut source).unwrap();
        assert_eq!(app.selected_agent(), Some(AgentId::from_u64(12)));
        assert!(rendered_workspace(&app, 100, 30).contains("› DB-02"));
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .unwrap();
        assert_eq!(app.agent, Some(AgentId::from_u64(12)));
        assert!(app.footer_text().contains("Esc Coordinator"));
        assert!(app.footer_text().contains("Tab Input"));
        let worker = rendered_workspace(&app, 100, 30);
        assert!(worker.contains("Viewing DB-02 · talk to Coordinator below"));
        assert!(worker.contains("Ask the Coordinator about DB-02"));
        app.key(KeyEvent::from(KeyCode::Esc), &mut source).unwrap();
        assert_eq!(app.agent, None);
        assert_eq!(app.focus, Focus::Input);
        let coordinator = rendered_workspace(&app, 100, 30);
        assert!(coordinator.contains(" COORDINATOR "));
        assert!(coordinator.contains("What would you like Orynth to do?"));
    }

    #[test]
    fn switcher_and_slash_commands_use_the_recovered_team() {
        let mut app = Workspace::new(navigation_snapshot());
        let mut source = EmptySource;
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        assert_eq!(app.overlay, Some(Overlay::AgentSwitcher));
        let switcher = rendered_workspace(&app, 100, 30);
        assert!(switcher.contains("SWITCH AGENT"));
        assert!(switcher.contains("SEC-03"));
        for _ in 0..3 {
            app.key(KeyEvent::from(KeyCode::Down), &mut source).unwrap();
        }
        assert_eq!(app.switcher_index, 3);
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .unwrap();
        assert_eq!(app.agent, Some(AgentId::from_u64(13)));
        assert_eq!(app.overlay, None);

        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        assert_eq!(app.switcher_index, 3);
        for _ in 0..3 {
            app.key(KeyEvent::from(KeyCode::Up), &mut source).unwrap();
        }
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .unwrap();
        assert_eq!(app.agent, None);
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        for _ in 0..3 {
            app.key(KeyEvent::from(KeyCode::Down), &mut source).unwrap();
        }
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .unwrap();
        assert_eq!(app.agent, Some(AgentId::from_u64(13)));

        for ch in "/agent AUTH-01".chars() {
            app.key(KeyEvent::from(KeyCode::Char(ch)), &mut source)
                .unwrap();
        }
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .unwrap();
        assert_eq!(app.agent, Some(AgentId::from_u64(11)));
        app.focus = Focus::Input;
        app.input = "/agent MISSING".to_owned();
        app.submit();
        assert!(
            app.notice
                .as_deref()
                .unwrap_or_default()
                .contains("No agent named MISSING")
        );
        assert_eq!(app.agent, Some(AgentId::from_u64(11)));
        app.input = "/coordinator".to_owned();
        app.submit();
        assert_eq!(app.agent, None);
        assert_eq!(app.focus, Focus::Input);
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        app.key(KeyEvent::from(KeyCode::Esc), &mut source).unwrap();
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn mock_work_requests_warn_without_hiding_navigation_or_help() {
        let mut app = Workspace::new(navigation_snapshot());
        app.input = "Make a personal website".to_owned();
        app.submit();
        let warning = app.notice.as_deref().unwrap_or_default();
        assert!(warning.contains("OFFLINE DEMO / MOCK MODE"));
        assert!(warning.contains("cannot execute new AI tasks"));
        assert!(warning.contains("Connect a model provider"));
        let rendered = rendered_workspace(&app, 100, 30);
        assert!(rendered.contains("Ctrl+P Agents"));
        app.overlay = Some(Overlay::Help);
        let help = rendered_workspace(&app, 100, 30);
        assert!(help.contains("Ctrl+P opens the agent switcher"));
        assert!(help.contains("Workers are read-only"));
        assert!(help.contains("Esc returns to Coordinator"));
    }

    #[test]
    fn scrolling_coordinator_requests_an_older_event_page() {
        struct PagingSource {
            snapshot: TuiSnapshot,
            page: Vec<StoredEvent>,
            request: Option<(Option<u64>, usize)>,
        }

        impl TuiDataSource for PagingSource {
            fn snapshot(&mut self) -> Result<TuiSnapshot, String> {
                Ok(self.snapshot.clone())
            }

            fn select_run(&mut self, _run_id: RunId) -> Result<(), String> {
                Ok(())
            }

            fn event_page(
                &mut self,
                before_sequence: Option<u64>,
                limit: usize,
            ) -> Result<Vec<StoredEvent>, String> {
                self.request = Some((before_sequence, limit));
                Ok(self.page.clone())
            }
        }

        let run_id = RunId::from_u64(1);
        let agent_id = AgentId::from_u64(2);
        let stored = |sequence| StoredEvent {
            sequence,
            event: Event::new(run_id, EventKind::AgentPaused { agent_id }),
        };
        let snapshot = TuiSnapshot {
            selected: Some(run_with_events(vec![stored(3), stored(4)])),
            runs: Vec::new(),
            presentation: None,
        };
        let mut source = PagingSource {
            snapshot: snapshot.clone(),
            page: vec![stored(1), stored(2)],
            request: None,
        };
        let mut app = Workspace::new(snapshot);
        app.focus = Focus::Conversation;
        app.key(KeyEvent::from(KeyCode::PageUp), &mut source)
            .expect("history should page");
        assert_eq!(source.request, Some((Some(3), EVENT_PAGE_SIZE)));
        assert_eq!(app.older_events.len(), 2);
        assert_eq!(app.older_events[0].sequence, 1);

        let deep_snapshot = TuiSnapshot {
            selected: Some(run_with_events((3..=130).map(stored).collect())),
            runs: Vec::new(),
            presentation: None,
        };
        let mut deep_source = PagingSource {
            snapshot: deep_snapshot.clone(),
            page: vec![stored(1), stored(2)],
            request: None,
        };
        let mut deep = Workspace::new(deep_snapshot);
        deep.focus = Focus::Conversation;
        deep.activity_limit = MAX_CONVERSATION_EVENTS;
        deep.key(KeyEvent::from(KeyCode::PageUp), &mut deep_source)
            .expect("full window should advance to older events");
        assert_eq!(deep.history_before, Some(3));
        assert_eq!(deep_source.request, Some((Some(3), EVENT_PAGE_SIZE)));
        assert_eq!(deep.coordination_window()[0].sequence, 1);
        deep.key(KeyEvent::from(KeyCode::End), &mut deep_source)
            .expect("End should return to current activity");
        assert_eq!(deep.history_before, None);
        assert_eq!(
            deep.coordination_window()
                .last()
                .map(|event| event.sequence),
            Some(130)
        );

        deep.older_events = (3..=514).map(stored).collect();
        deep.history_before = Some(4);
        deep.load_older_if_needed(&mut deep_source)
            .expect("a full cache should slide toward older events");
        assert_eq!(deep.older_events.len(), MAX_OLDER_EVENTS);
        assert_eq!(
            deep.older_events.first().map(|event| event.sequence),
            Some(1)
        );
        assert_eq!(
            deep.older_events.last().map(|event| event.sequence),
            Some(512)
        );
    }

    #[test]
    fn active_failure_without_conflict_has_inspectable_issue() {
        let run_id = RunId::from_u64(1);
        let agent_id = AgentId::from_u64(2);
        let mut failures = FailureMemory::default();
        failures
            .apply(&FailureTransition::Recorded {
                record: FailureRecord::new(
                    agent_id,
                    "auth-test",
                    "Run auth tests",
                    "A session assertion failed",
                ),
            })
            .expect("valid failure");
        let run = RecoveredRun {
            run_id,
            state: RuntimeState {
                run_id,
                status: RunStatus::Active,
                tasks: Default::default(),
                agents: Default::default(),
                artifacts: Default::default(),
                events_applied: 0,
            },
            context: ContextGraph::new(),
            events: Vec::new(),
            cache_telemetry: CacheTelemetry::new(),
            messages: Vec::new(),
            assumptions: AssumptionGraph::new(),
            scheduler: SchedulerState::default(),
            capabilities: CapabilityPolicy::new(),
            tools: ToolHistory::new(),
            failures,
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
                active_failure_count: 1,
            },
        };
        let app = Workspace::new(TuiSnapshot {
            selected: Some(run),
            runs: Vec::new(),
            presentation: None,
        });
        assert_eq!(app.issue_count(), 1);
        let detail = app
            .issue_lines()
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(detail.contains("A session assertion failed"));
        assert!(!detail.contains("No unresolved conflict"));
    }

    #[test]
    fn each_active_failure_can_be_selected_from_the_team() {
        let agent_id = AgentId::from_u64(2);
        let mut run = run_with_events(Vec::new());
        for (fingerprint, reason) in [("first", "First failure"), ("second", "Second failure")] {
            run.failures
                .apply(&FailureTransition::Recorded {
                    record: FailureRecord::new(agent_id, fingerprint, "Run tests", reason),
                })
                .expect("valid failure");
        }
        run.manager.active_failure_count = 2;
        let mut app = Workspace::new(TuiSnapshot {
            selected: Some(run),
            runs: Vec::new(),
            presentation: None,
        });
        assert_eq!(app.issue_count(), 2);
        app.selected_team = 1;
        let detail = app
            .issue_lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(detail.contains("Second failure"));
    }

    #[test]
    fn empty_and_tiny_workspace_are_honest() {
        let empty = empty();
        let normal = render_workspace_for_terminal(&empty, 100, 28);
        assert!(normal.contains("COORDINATOR"));
        assert!(normal.contains("NO ACTIVE RUN"));
        assert!(normal.contains("What would you like Orynth to do?"));
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
    fn bracketed_paste_inserts_multiline_unicode_at_cursor_without_submit() {
        let mut app = Workspace::new(empty());
        app.input = "Build  tracker".to_owned();
        app.input_cursor = 6;
        app.paste("a\r\nb\n🙂\tcode\u{1b}[31m");
        assert_eq!(app.input, "Build a\nb\n🙂\tcode tracker");
        assert_eq!(app.input_cursor, "Build a\nb\n🙂\tcode".chars().count());
        assert!(app.notice.is_none());
    }

    #[test]
    fn oversized_paste_is_rejected_without_truncation() {
        let mut app = Workspace::new(empty());
        app.paste(&"x".repeat(MAX_INPUT_CHARS + 1));
        assert!(app.input.is_empty());
        assert!(
            app.notice
                .as_deref()
                .is_some_and(|notice| notice.contains("too large"))
        );
    }

    #[test]
    fn paste_only_targets_editable_input_focus() {
        let mut app = Workspace::new(empty());
        app.focus = Focus::Team;
        let before = app.input.clone();
        if app.focus == Focus::Input {
            app.paste("should not arrive");
        }
        assert_eq!(app.input, before);
    }

    #[test]
    fn enter_submits_one_complete_multiline_paste() {
        let mut app = Workspace::new(empty());
        let mut source = LiveSource {
            submitted: Vec::new(),
        };
        let prompt = "Build a task tracker.\n\n- localStorage\n- filters";
        app.paste(prompt);
        app.key(KeyEvent::from(KeyCode::Enter), &mut source)
            .expect("submit should be handled");
        assert_eq!(source.submitted, vec![prompt.to_owned()]);
        assert!(app.input.is_empty());
    }

    #[test]
    fn pasted_input_height_is_bounded_and_preserves_newlines() {
        let text = "line\n".repeat(100);
        assert_eq!(sanitize_paste(&text), text);
        assert!(input_height(&text, 80) <= MAX_INPUT_LINES as u16);
        assert!(wrapped_input_lines("a\nb", 80) >= 2);
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

    #[test]
    fn input_edits_unicode_at_character_boundaries() {
        let mut app = Workspace::new(empty());
        for ch in "a界z".chars() {
            app.input_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        app.input_key(KeyEvent::from(KeyCode::Left));
        app.input_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.input, "az");
        app.input_key(KeyEvent::from(KeyCode::Char('🙂')));
        assert_eq!(app.input, "a🙂z");
        app.input_key(KeyEvent::from(KeyCode::Home));
        app.input_key(KeyEvent::from(KeyCode::Delete));
        assert_eq!(app.input, "🙂z");
    }

    #[test]
    fn workspace_controls_keep_input_available_in_worker_view() {
        let mut app = Workspace::new(empty());
        let mut source = EmptySource;
        app.agent = Some(AgentId::from_u64(1));
        app.key(KeyEvent::from(KeyCode::Char('/')), &mut source)
            .unwrap();
        assert_eq!(app.input, "/");
        app.key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        assert!(!app.team_visible);
        assert!(app.footer_text().contains("Ctrl+B Show team"));
        app.key(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut source,
        )
        .unwrap();
        assert_eq!(app.overlay, Some(Overlay::Palette));
        app.key(KeyEvent::from(KeyCode::Esc), &mut source).unwrap();
        assert_eq!(app.overlay, None);
        app.key(KeyEvent::from(KeyCode::Esc), &mut source).unwrap();
        assert_eq!(app.agent, None);
    }

    #[test]
    fn narrow_and_long_unicode_input_render_without_panicking() {
        let mut app = Workspace::new(empty());
        app.input = "界🙂".repeat(200);
        app.input_cursor = app.input.chars().count();
        for (width, height) in [(38, 10), (60, 20), (120, 34)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| app.render(frame)).unwrap();
        }
    }

    #[test]
    fn work_tabs_start_at_top_while_conversation_follows_bottom() {
        let mut app = Workspace::new(empty());
        app.agent = Some(AgentId::from_u64(1));
        app.scroll = 4;
        app.shift_tab(1);
        assert_eq!(app.tab, AgentTab::Conversation);
        assert_eq!(app.scroll, 0);
        assert_eq!(AgentTab::Conversation.compact_label(), "Chat");
    }

    #[test]
    fn coordinator_history_expands_only_when_scrolled() {
        let mut app = Workspace::new(empty());
        assert_eq!(app.activity_limit, INITIAL_CONVERSATION_EVENTS);
        app.focus = Focus::Conversation;
        app.key(KeyEvent::from(KeyCode::PageUp), &mut EmptySource)
            .expect("scroll works");
        assert_eq!(app.activity_limit, INITIAL_CONVERSATION_EVENTS + 8);
        assert!(app.pinned_scroll);
    }

    #[test]
    fn new_activity_does_not_move_a_scrolled_up_reader() {
        let mut app = Workspace::new(empty());
        app.pinned_scroll = true;
        app.scroll = 7;
        assert_eq!(app.conversation_offset(12), 7);
        assert_eq!(app.conversation_offset(18), 7);
        app.pinned_scroll = false;
        assert_eq!(app.conversation_offset(18), 18);
    }

    #[test]
    fn conversation_code_blocks_keep_structure_and_unicode() {
        let lines = conversation_content_lines("Use this:\n```rust\nlet id = \"🧭\";\n```\nDone.");
        let rendered = lines.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("┌─ rust")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("│ let id = \"🧭\";"))
        );
        assert!(rendered.iter().any(|line| line.contains("└─")));
        assert!(rendered.iter().any(|line| line.contains("Done.")));
    }
}
