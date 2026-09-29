# Orynth TUI Implementation Report

## Phase F.3 chat-first Workspace: offline retest build

The navigation retest found that seeing AI Team did not teach users how to
open a worker. The focused follow-up makes Tab focus explicit in the footer,
highlights the team border, title, instruction row, and selected agent, and
adds Ctrl+P switcher navigation over the recovered team. Worker input names
the viewed agent and says the input still goes to Coordinator. The demo header
and free-form task warning identify OFFLINE DEMO / MOCK MODE. The requested
release walkthrough passed through DB-02, Coordinator, SEC-03, AUTH-01, and
Coordinator without documentation. The focused TUI and app suites pass 26/26
and 9/9; formatting and changed-crate Clippy pass.

The primary CLI entry now opens a Workspace shell over the same authoritative
`TuiDataSource` used by the Phase F.2 debugger. A 75/25 wide layout gives the
Coordinator pane priority. The right side shows worker names, roles, status,
health, and effective models. Worker views expose Work, read-only
Conversation, agent-visible Context, Tools, and Access. Runtime conflicts open
a human-readable overlay. Ctrl+K shows implemented navigation commands, and
`/debug` switches to the preserved Advanced Debugger.

The Workspace narrates selected structured runtime events. The demo now
records a user goal and a conflict-derived Coordinator summary as versioned
conversation turns. It also records an AUTH-01 question to DB-02 and DB-02's
answer through runtime IPC, alongside the persisted conflict. The main pane
keeps the recorded goal, team, and conflict visible at 80 columns. A second
visual pass shortened the 80-column team rows and made the 10-agent sidebar
page around the selection. The empty run uses the whole main pane.

Input has Unicode-safe insertion, deletion, cursor movement, and bounded
history. It remains a Coordinator control surface while inspecting a worker.
Ctrl+B collapses the team. The issue is an Enter-selectable sidebar item.
`/runs` and its palette action open the debugger run list. Both modes use a
shared semantic theme and one `TuiDataSource`.

Natural-language input is rejected transparently offline. Live submission,
real stream projection, and model-backed orchestration remain future runtime
work. Idle refreshes reuse a selected SQLite run's recovered projection when
its event count is unchanged. A changed run still reconstructs its complete
history before bounding the UI event window; incremental recovery remains a
performance follow-up before large live runs.

The latest follow-up allows the bounded Coordinator activity window to expand
from two to 128 events on scroll and makes an active failure inspectable in the
attention panel when no conflict exists. The empty-state test now checks the
intentional full-width layout. A further follow-up anchors scrolled-up
conversation content when new events arrive. The focused TUI suite now passes
26/26, including navigation, issue selection, scrolling, code blocks, and older-page
regressions. The full
all-features workspace test remains incomplete because Windows Application
Control intermittently blocks generated test executables (OS error 4551).
Changed-crate Clippy with warnings denied passes; workspace-wide Clippy is
blocked while loading a dependency in the unchanged plugin-MCP crate. A later `cargo fmt --all`
attempt succeeded after earlier Application Control blocks. The rebuilt release
demo rendered the compact
first frame at 80 columns with the Coordinator goal, team, conflict, and input
visible. Opening AUTH-01 and switching through its recorded Conversation and
Tools tabs worked in the PTY; Ctrl+C restored the terminal.

The next history pass adds a source-backed older-activity request to Coordinator
scrolling. SQLite serves it with an indexed `ORDER BY sequence DESC LIMIT`
query and checksum-verified decoding. Run-list status and counts also avoid
loading every run's full event vector. The Workspace retains at most 512 older
raw events and renders at most 128 coordination items. At the top of that
window, PageUp advances a sequence cursor and replaces newer cache pages with
older ones; End returns to current activity. The SQLite selected-run snapshot
still fully reconstructs changed runs. Focused SQLite paging, TUI, and CLI tests pass;
the current workspace-wide gate is still subject to Windows Application
Control.

ADR-0069 defines the durable complete-turn payload. Runtime validation rejects
empty or oversized turns, unknown Coordinator agents, and child-agent
impersonation; recovery rejects malformed payloads. SQLite reopen reconstructs
recorded turns. The demo's final summary is assembled from the recovered
conflict and recorded by the same runtime API. Provider streaming still needs
a chunk and completion contract tied to a turn event ID.

The current release PTY walkthrough completed Coordinator → AUTH-01 Work →
Conversation → Context → Tools → Coordinator → issue overlay → command palette
→ Advanced Debugger → Workspace → quit. The debugger retained its technical
Overview, and `Ctrl+W` returned to the Workspace; terminal state was restored
on quit. Fenced code in recorded conversation and worker exchanges now receives
a compact code rail and background, with a Unicode regression test.
The worker Access tab now lists recorded capability leases, their task scope,
network lease presence, owned resources, and descriptive specialist scope.
It avoids implying that a declared scope alone grants permission.
The attention sidebar now selects each active conflict or failure separately;
the overlay follows the selected runtime record. A two-failure regression
checks that selecting the second item opens its own reason.
The Work tab shows the latest recorded exchange and an active failure reason.

Validation: release build, format check, and changed-crate Clippy pass. The
release demo, empty Workspace, and 10-agent demo rendered in a PTY; Ctrl+C
restored the terminal. The latest app, event-store, runtime, and TUI suites
passed 9/9, 61/61, 37/37, and 26/26 after intermittent Windows Application
Control blocks. The full workspace test and Clippy commands remain blocked
while loading the `url` dependency's `zerofrom_derive` DLL (OS error 4551).

## Phase F.2 redesign

The second UX pass turns the F.1 debugger into one connected control room.
The header now separates product identity, human run name, status, and
runtime counters from technical identity. Navigation reads Overview, Team,
Activity, Knowledge, Messages, Tools, Access, Conflicts, and Runs. Overview
is composed as a single showcase: AI Team and Needs Attention share the main
work area, with Recent Activity below; normal-width terminals stack the same
three sections in the same reading order.

The reusable event presentation model now carries severity, actor, related
entities, sequence, and occurrence metadata in addition to its deterministic
title, summary, explanation, and technical kind. Activity rows use that model
for readable titles and restrained severity color. Optional human run names
are kept separate from authoritative `RunId` values.

The demo publishes its authentication schema as shared project knowledge so
the default manager view has a meaningful Knowledge screen; private-context
visibility rules remain unchanged for real runs. Common namespaces are also
given readable names in the human view, with their raw namespace retained in
the inspector.

## Implemented Views

The Phase F.2 control room uses Ratatui bordered panels, tables, lists, tabs,
wrapping, selection highlights, and centered overlays. The screens are
Overview, Team, Activity, Knowledge, Messages, Tools, Access,
Conflicts, and Runs. Primary views use plain language; technical values are
kept in detail overlays.

## Keyboard Controls

`↑`/`↓` and `j`/`k` navigate; `Enter` opens the selected item; `Tab` and
`Shift-Tab` change screens; `1`-`9` jump; `/` filters; `r` refreshes; `[`/`]`
page older/newest Activity; `a` toggles Conflicts/Assumptions; `s` selects a
run; `?` opens contextual help; `Esc` closes overlays; `q` and `Ctrl-C` exit.
The footer is dynamic and does not advertise unavailable actions.

## Runtime Integration

`orynth tui` supplies a `TuiDataSource` to `orynth-tui`. SQLite recovery uses
the authoritative event store and `RuntimeService`; the TUI owns only
selection, filtering, pagination, and presentation state. It cannot call a
provider, append an event, execute a tool, or change runtime authority.

## Demo Mode

`orynth tui --demo` presents **Authentication Migration Demo** with a compact
first-run overlay. The description explains the real scenario: MANAGER
coordinates authentication/database work, AUTH-01 handles authentication,
DB-02 checks migration constraints, and SEC-03 reviews security. AUTH-01 and
DB-02 have recorded conflicting `users.id` assumptions. The runtime also
contains real model selection/switching, knowledge, messages, health, budgets,
ownership, a capability lease, cache telemetry, and a verified read-only tool
transaction. `ORYNTH_TUI_DEMO_AGENTS=10` is the bounded stress scenario.

## Interactive Actions

All advertised actions do something: navigation changes screen or selection;
Enter opens an inspector for agents, events, messages, knowledge blocks,
tools, permissions, conflicts, assumptions, and runs; `s` selects a run; `r`
refreshes; paging changes the visible event window; and help/Esc/q behave as
shown. Run selection remains an explicit read-only source operation.

## Read-only Areas

Pause/resume/cancel, model switching, capability grants, ownership changes,
tool approval/execution, provider calls, live breakpoint actions, replay
execution, and fork execution are not exposed. The existing CLI commands
remain explicit workflows.

## Event Paging

Activity uses a newest 512-event display window and requests older bounded
pages from SQLite only on demand. `]` returns to the newest page. Each frame
renders only bounded visible rows and the selected detail; large raw payloads
are truncated in the inspector.

## Context Inspection

Knowledge projects through the selected logical agent or runtime principal
using the context graph's existing visibility, trust, block, and token rules.
Lifecycle labels are translated to Active, Outdated, Archived, Invalid, and
Replaced, with technical lifecycle values available in details.

## IPC Inspection

Messages are presented as routes such as `MANAGER → AUTH-01`, with QUESTION,
CONFLICT WARNING, ANSWER, or other readable message kinds, title, body, and
why it matters. Raw typed payload, message ID, provenance, and trust remain in
the detail overlay.

## Tool Inspection

Tool actions show the human action name, responsible agent, resource, result,
permission, ownership, risk boundary, and undo availability. Transaction ID,
tool ID, effect state, policy information, repair, and preview remain
technical details. No tool effect is reachable from the TUI.

## Permission/Ownership Inspection

Permissions explains “Can access” and “Owns” per human-readable agent. Domain
labels use file access, process execution, network access, and similar terms;
capability leases, task scopes, canonical resources, and expiry remain in
details. Network is described as blocked unless a recorded scoped lease says
otherwise.

## Error Handling

Source failures remain in the status line without fabricated data. Empty runs,
unknown runs, missing projections, narrow terminals, unavailable older pages,
UTF-8 truncation, and out-of-range selections produce bounded messages.
Errors from input or drawing unwind through terminal cleanup.

## Terminal Cleanup

The terminal guard enters raw mode, alternate-screen mode, and hidden-cursor
mode. It restores all three on normal exit and through `Drop`, including
input/draw error paths and panic unwinding.

## Tests

Focused tests cover Ratatui rendering, small terminals, real run identity,
empty navigation, agent/knowledge/permission selection, detail scrolling,
human-detail targets for events/messages/knowledge/tools/conflicts/runs,
help/Esc/Tab/q controls, projection rendering, and semantic breakpoint
scanning. Operator tests cover demo recovery, persisted run selection and
event paging, SQLite inspection, replay, fork, diff, and the read-only debug
session. Full validation is recorded in the final handoff.

## F.2 validation

The following gates pass in the current worktree:

- `cargo fmt --all -- --check`
- `cargo check --workspace`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test -p orynth-tui`
- `cargo test -p orynth`
- `cargo build --release --workspace`
- release smoke launch with `.\target\release\orynth.exe tui --demo`, including
  first-run dismissal and clean `q` exit/terminal restoration.

The exact `cargo test --workspace --all-features` command passes the application,
agent, assumptions, cache, CLI, context, and event-store test groups observed
before the host terminated the remaining test process with Windows status
`0xC000013A`. It is therefore reported as environment-incomplete rather than
as a full-suite pass; the focused TUI and operator suites pass independently.

## Memory Measurements

Release Windows working-set samples after the UX overhaul. These are stable
working-set readings taken about 1.2 seconds after launch on the current
development host; they are not a cross-platform memory guarantee.

| Scenario | Working set |
|---|---:|
| Empty SQLite database | 7,676 KiB |
| Offline demo, 4 agents | 7,624 KiB |
| Offline demo, 10 agents | 7,812 KiB |

The previous Phase F baseline was 6,920 KiB empty, 6,944 KiB for four demo
agents, and 7,064 KiB for ten demo agents. The post-polish sample is within
the same bounded range; the Ratatui presentation layer adds no unbounded
growth in the ten-agent stress scenario.

## Known Limitations

SQLite recovery still loads the selected stream before display paging; older
pages are bounded but are not yet a streaming database cursor. Live refresh is
manual, terminal glyph width is conservative, and persisted runs without a
domain title are shown as numbered sessions until selected.
The current run status is intentionally a compact state badge; elapsed time
is not shown when the runtime does not provide a trustworthy duration.

## Deferred Features

Safe live runtime controls, provider/plugin process inspection, artifact
browser and retention controls, live subscriptions, semantic breakpoint
actions, true cursor-based event streaming, replay/fork UI workflows, and
hosted cross-platform memory measurements remain deferred.
