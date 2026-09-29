# Orynth TUI design

Phase F.3 introduces a chat-first Workspace as the primary interface. The
Phase F.2 control room remains available as Advanced Debugger. Both render
authoritative runtime projections. The runtime persists complete
user/Coordinator turns in the run event log. Phase G adds preliminary live
submission and streaming through an app-supplied data source; the widgets
remain presentation-only.

## Normal Workspace

The shell presents a Coordinator pane, AI Team sidebar, input area, header,
and compact footer. On wide terminals, the main pane gets about 75% of the
width; at 72–103 columns the team gets a fixed 28 columns. On narrower
terminals, team focus temporarily shows the team in the main area. Tiny
terminals show a resize message. With no active run, the empty state uses
the full width and points to `orynth --demo`.

The main pane translates selected runtime events into deterministic
coordination activity. Worker views provide Work, Conversation, Context,
Tools, and Access tabs. The sidebar shows logical worker identity, role,
lifecycle, health, and effective model. Conflicts open a plain-language
overlay. Technical identifiers stay in Advanced Debugger.
Access reads the actual capability leases and ownership projection. Lease
resources and task scope are shown separately from the specialist's declared
work scope; a missing network lease is stated plainly.
Each recorded conflict or active failure is an individually selectable
attention item. The compact sidebar shows the selected issue and its position
among the current issues; Enter opens details from that exact runtime record.

Focus rotates between input, team, and conversation with Tab/Shift-Tab. Team
focus has a bright full border, a focused title, an instruction row, and a
high-contrast selected row. Up/Down select agents or the issue when the team
has focus and scroll when conversation has focus. Enter opens the selection.
Ctrl+P opens a compact switcher derived from the recovered team; it includes
Coordinator once and accepts arrows, Enter, and Esc. `/agent NAME` accepts a
worker name and reports missing or invalid names without changing the view.
`/coordinator` returns to the primary conversation. Left/Right changes worker
tabs. Esc returns to Coordinator. Ctrl+B collapses the team. Ctrl+K opens
implemented commands. `/debug` enters Advanced Debugger; `/runs` opens its run
history. Ctrl+W returns to Workspace. The input remains usable while inspecting
a worker; its title and placeholder say that messages still address the
Coordinator. The demo is labeled **OFFLINE DEMO / MOCK MODE** in the header;
free-form requests receive a deterministic warning that mock agents cannot
execute new AI tasks. Input editing is Unicode-safe, with cursor movement,
deletion, bounded local history, and terminal bracketed-paste support. Paste
is inserted into the focused editor as one bounded, sanitized edit: embedded
newlines remain editable input and never submit automatically. The Coordinator
input grows to a bounded six-line viewport and keeps Enter as the explicit
submit action. Bracketed-paste mode is restored on exit, and Orynth does not
capture mouse selection.

The shell refreshes its source every two seconds. New runtime events show a
notice when the conversation is scrolled up and keep its absolute wrapped-line
offset until the user follows the bottom again. The Coordinator activity window
grows with new events while pinned, up to its 128-event limit. PageUp requests
older raw events through a bounded source page when the current window has too
few coordination events. At the top of a full 128-item window, PageUp advances
a sequence cursor into older activity; End returns to current activity. SQLite
answers page requests with an indexed bounded query; the UI retains at most
512 older raw events while moving the window backward. The rendered conversation
starts with two recent activity items to keep the first screen legible and
expands to at most 128 items when the user scrolls upward. The rendered window
and team rows are bounded; the 10-agent sidebar scrolls around the
selected agent. Runtime entities are formatted in the TUI presentation layer,
using a shared semantic theme. No model, tool, or permission mutation happens
in the UI.

Natural-language submission remains unavailable offline because no provider
is present. The UI explicitly reports
that such text was not sent or recorded. Input history is local to the current
session. This is an implementation gap, not a complete chat experience. The
Coordinator pane combines durable user/Coordinator turns with a deterministic
summary of recovered task, team, issue, and event state. Recorded worker IPC
appears as read-only conversation.
Fenced code in recorded turns or worker exchanges receives a restrained code
rail and background; content remains plain text and wraps at the pane edge.
The retained older-history window is finite, but the sequence cursor can move
beyond it by replacing newer cached pages. Returning to live activity uses End;
incremental recovery of a changed selected run remains open. Later
provider streaming can update the same snapshot/projection boundary with a
chunk contract referring to the starting turn event ID.

## Advanced Debugger (Phase F.2)

## Shell

Every screen uses the same order:

1. Header: Orynth identity, human run name, status, team count, issue count,
   and event count.
2. Navigation: Overview, Team, Activity, Knowledge, Messages, Tools, Access,
   Conflicts, and Runs.
3. Primary work area: the content that answers the screen's human question.
4. Contextual activity or detail: a selected item, explanation, or timeline.
5. Footer: only the controls currently available.

## Human-first mapping

| Human concept | Runtime source | Technical detail |
|---|---|---|
| Team member | manager agent projection | AgentId, model reference, revisions |
| Knowledge | context projection | block ID, scope, trust, content hash |
| Message | typed IPC projection | message ID, provenance, typed payload |
| Access | capability and ownership projections | leases, canonical resources |
| Activity | event presentation model | event kind, sequence, raw payload |
| Conflict | assumption graph projection | assumption/conflict IDs and revisions |

Default screens prefer names, roles, missions, models, health, and plain
English explanations. Inspectors retain the exact runtime identifiers and
payloads.

## Overview

Overview is the screenshot test. It presents the AI Team, the current
attention item, and recent activity as a single narrative. Wide terminals use
Team and Attention side-by-side with Activity below. Normal terminals stack
those panels in the same order. Very small terminals show a bounded resize
message instead of attempting a partial layout.

## Interaction

Arrow keys or `j`/`k` move, `Enter` opens a reusable inspector, `Tab` changes
sections, `1`-`9` jumps to a section, `/` filters the current list, `?` opens
contextual help, `Esc` closes overlays, and `q`/`Ctrl-C` exits. Search and
selection are presentation state only. Display names are never runtime
authority.

## Visual language

The palette is restrained: cyan/blue for brand and information, green for
healthy or successful state, yellow for attention, red for conflict/failure,
and muted gray for supporting metadata. Symbols accompany colors so meaning
survives limited color terminals. Rounded bordered panels group related
information; selection uses a subdued surface highlight and focus is shown by
the active panel title/border.

## Boundaries

No screen invokes a provider, executes a tool, changes a model, grants access,
mutates context, pauses an agent, or appends an event. Runtime truth remains
in the event store and recovered projections; the TUI owns only bounded
navigation, filtering, and presentation state.
