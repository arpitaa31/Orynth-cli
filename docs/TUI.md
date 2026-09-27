# Orynth TUI

Status: chat-first Workspace redesign in progress. Phase F.2 Advanced
Debugger remains available. Deep Audit #2 has not started.

## Primary Workspace

`orynth` opens the normal Workspace. `orynth --demo` opens a deterministic
offline team scenario. `orynth debug --demo` opens Advanced Debugger directly.
The previous line-oriented debug session is `orynth debug-session --db <path>
--run <id>`.

The Workspace shows Coordinator activity, an AI Team sidebar on wide
terminals, worker Work/Conversation/Context/Tools/Access tabs, and a contextual
issue overlay. Use Tab to focus the team, Up/Down to select, Enter to open a
worker, Left/Right to change tabs, and Esc to return to Coordinator. Ctrl+K
opens the command palette. `/help`, `/agents`, `/agent NAME`, `/status`,
`/conflicts`, `/tools`, `/context`, `/runs`, `/coordinator`, and `/debug` are
implemented. Ctrl+W returns from the in-app debugger.

This is an offline inspection surface. No provider is connected, and natural
language entered in the input is explicitly reported as unsent and unrecorded.
Durable conversation, streaming, runtime-backed input, and visual/manual
acceptance remain open work.

## Advanced Debugger

The TUI is a read-only runtime control room over authoritative
`RecoveredRun` projections. The default presentation is human-readable;
technical IDs, raw payloads, revisions, hashes, trust, and canonical
resources appear in inspection overlays rather than every primary list.

## Launch

```text
cargo run -p orynth -- tui --demo
.\target\release\orynth.exe tui --demo
cargo run -p orynth -- tui --db .orynth/runtime.db [--run <run-id>]
```

The deterministic offline demo is titled **Authentication Migration Demo**.
It explains that a manager is coordinating authentication/database work while
AUTH-01 and DB-02 disagree about `users.id`; SEC-03 reviews security. The
runtime generates real agents, models, knowledge, messages, conflicts,
health, budgets, ownership, permissions, cache observations, and tool
transactions. `ORYNTH_TUI_DEMO_AGENTS=10` remains available for measurement.

## Screens

| Screen | Human purpose | Technical equivalent |
|---|---|---|
| Overview | What is happening now, team, warnings, recent activity | Manager projection, conflicts, recovered events |
| Team | Responsibilities, model, lifecycle, and health | Logical `AgentId`, `AgentStatus`, `HealthStatus`, `ModelRef` |
| Activity | Plain-English runtime timeline | Event store `EventKind` and sequences |
| Knowledge | Project information available to agents | Context graph blocks, lifecycle, scope, trust |
| Messages | Agent-to-agent conversation | Typed IPC envelopes and provenance |
| Tools | Actions, results, and verification | Tool transactions, policy, repair, preview |
| Access | What agents may access and own | Capability leases and scheduler ownership |
| Conflicts | Side-by-side disagreements | Assumptions, conflict IDs, revisions, evidence |
| Runs | Recognizable persisted sessions | SQLite run IDs, status, and event history |

## Controls

`↑`/`↓` or `j`/`k` moves; `Enter` opens the selected human summary and
technical details; `Tab`/`Shift-Tab` changes screens; `1`-`9` jumps directly;
`/` filters; `r` refreshes authoritative state; `[`/`]` pages older/newest
Activity; `a` toggles Conflicts/Assumptions; `s` selects a run on Runs; `?`
opens contextual help; `Esc` closes help/details; `q` or `Ctrl-C` exits.

The footer is dynamic. It advertises `Enter` only when the current screen has
an inspectable item, and only advertises Activity paging, run selection, or
assumption toggling where those actions work.

While an overlay is open, the footer switches to the actions that apply to
that overlay: `Enter`/`Esc` close detail, `↑`/`↓` scroll detail, and `Enter` or
`Esc` dismisses the welcome panel. Screen-only actions are not advertised
behind an overlay.

## Detail overlays

The reusable bordered inspector overlay is used for agents, events, messages,
knowledge blocks, tools, permissions/agents, conflicts, assumptions, and
runs. It presents a deterministic plain-English summary first, then a
**Technical Details** section. Up/Down scrolls long content and `Esc` closes
the overlay.

## Responsive layout and visual language

Ratatui bordered panels, tables, lists, tabs, wrapping, selection highlights,
and centered overlays are used throughout. Wide terminals use two-column
control-room panels; narrower terminals stack them; tiny terminals receive a
bounded resize message. Cyan marks navigation, green healthy/completed,
yellow attention, red conflicts/failures, and muted gray technical metadata;
status text and symbols accompany color.

## Read-only boundary

The TUI never invokes providers, appends events, executes tools, grants or
revokes permissions, changes models, pauses agents, or performs replay/fork
mutations. Existing CLI replay, fork, diff, and terminal debug commands remain
explicit workflows. Live subscriptions, live breakpoint actions, and safe
runtime mutation controls are deferred.

## Phase F.2 control-room design

The shell is ordered as header, navigation, primary work area, optional
activity context, and footer. The header presents `ORYNTH`, the runtime
control-room identity, the human run name, status, team count, issue count,
and event count; authoritative run IDs remain in inspectors.

The showcase Overview reads left-to-right and then top-to-bottom: **AI TEAM**
shows named workers, roles, models, lifecycle, and health; **ATTENTION
NEEDED** explains the current disagreement in everyday language; **RECENT
ACTIVITY** gives the ordered story of what Orynth observed. On narrower
terminals those panels stack without changing the story.

Primary labels are human concepts: Overview, Team, Activity, Knowledge,
Messages, Tools, Access, Conflicts, and Runs. IPC, capabilities, context
blocks, assumptions, event kinds, and leases remain available in subtitles or
inspectors rather than being required to read a screen.

`RunSummary.display_name` is optional presentation metadata. It improves run
recognition in the UI but is never used for runtime authority or selection;
the raw `RunId` remains available under Technical Details.
