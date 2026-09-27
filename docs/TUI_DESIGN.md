# Orynth TUI design

Phase F.3 introduces a chat-first Workspace as the primary interface. The
Phase F.2 control room remains available as Advanced Debugger. Both are
read-only presentations of authoritative runtime projections while durable
Coordinator conversation support is being designed.

## Normal Workspace (in progress)

The shell presents a Coordinator pane, AI Team sidebar, input area, header,
and compact footer. On terminals at least 82 columns wide, the main pane gets
75% of the width and the team gets 25%. On narrower terminals, team focus
temporarily shows the team in the main area. Tiny terminals show a resize
message.

The main pane translates selected runtime events into deterministic
coordination activity. Worker views provide Work, Conversation, Context,
Tools, and Access tabs. The sidebar shows logical worker identity, role,
lifecycle, health, and effective model. Conflicts open a plain-language
overlay. Technical identifiers stay in Advanced Debugger.

Focus rotates between input, team, and conversation with Tab/Shift-Tab.
Up/Down select agents when the team has focus and scroll when conversation has
focus. Enter opens a selected worker. Left/Right changes worker tabs. Esc
returns to Coordinator. Ctrl+K opens implemented commands. `/debug` enters
Advanced Debugger; Ctrl+W returns to Workspace.

Natural-language submission remains unavailable offline because no durable
Coordinator turn contract or provider is present. The UI explicitly reports
that such text was not sent or recorded. Input history is local to the current
session. This is an implementation gap, not a complete chat experience.

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
