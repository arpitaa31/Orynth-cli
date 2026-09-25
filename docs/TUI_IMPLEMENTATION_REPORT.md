# Orynth TUI Implementation Report

## Implemented Views

The full-screen client has dashboard, agent tree, selected agent, bounded
event timeline/details, context, typed IPC/messages, tools/transactions,
permissions/ownership/budgets, assumptions/conflicts, persisted runs, and
help views. The header reports the real run identity, lifecycle state, total
events applied, agent count, and warning count. Empty and small-terminal
states are explicit.

## Keyboard Controls

`1`-`9` select numbered views; `Tab`/`Shift-Tab` cycles views; arrows and
`j`/`k` move; `Home`/`End` jump; `Enter` selects a run or opens event details;
`/` starts filtering; `r` refreshes; `R` opens runs; `?` opens help; `Esc`
closes help/details; `q` and `Ctrl-C` exit. `[` loads an older event page and
`]` returns to the newest page.

## Runtime Integration

`orynth tui` passes a `TuiDataSource` into `orynth-tui`. The SQLite source
lists persisted run IDs, recovers the selected event stream through
`RuntimeService`, and refreshes from the event store. The TUI itself owns no
runtime authority and has no provider, tool-effect, permission, or event
append path.

## Demo Mode

`orynth tui --demo` builds a deterministic in-memory runtime with real
events and projections: one manager, three specialists, model selection and
switching, context visibility, IPC, assumptions and a conflict notification,
health, budgets, ownership, a capability lease, cache telemetry, and a
proposed/approved/verified read-only tool transaction. The optional
`ORYNTH_TUI_DEMO_AGENTS=10` scenario adds bounded specialists for responsiveness
measurement without inventing a separate UI data model.

## Interactive Actions

Supported actions are view navigation, run selection, refresh, filtering,
event detail inspection, and event page navigation. These are client-side
read-only operations.

## Read-only Areas

Pause/resume/cancel, model switching, capability grants, ownership changes,
tool approval/execution, replay execution, fork execution, live breakpoint
actions, and provider calls are intentionally unavailable. Existing replay,
fork, diff, and debug CLI commands remain separate, explicit workflows.

## Event Paging

The selected recovery retains the newest bounded event window (512 records).
The event view requests older pages from SQLite on demand, each bounded to
512 records, and can return to the newest window. Rendering caps visible rows
and detail text; it never formats the entire history per frame. Demo and
non-paging sources retain the bounded snapshot behavior.

## Context Inspection

Context inspection projects through the selected logical agent or runtime
principal with the context graph's existing visibility and trust rules. The
TUI requests bounded namespace/block and token limits and displays freshness,
active/stale state, and pressure without changing the graph.

## IPC Inspection

Messages are decoded from the recovered runtime projection and displayed with
sender, receiver, message kind, subject/detail, and provenance. The TUI does
not enqueue, acknowledge, or send messages.

## Tool Inspection

Tool records are recovered from durable transitions and displayed with
transaction ID, tool name, state, agent, and bounded detail. Proposal,
approval, preflight, verification, compensation, and failure states are
inspection data only; no external effect is reachable from the TUI.

## Permission/Ownership Inspection

The policy view reports recovered capability leases, agent/task scope,
resource ownership, budgets, usage, health, and model assignment. It keeps
logical `AgentId` distinct from the current effective model and does not
grant or revoke authority.

## Error Handling

Source errors remain in the status line and do not fabricate replacement
runtime data. Unknown runs, empty databases, missing projections, malformed
selection, narrow terminals, UTF-8 boundaries, and unavailable older pages
have bounded user-visible messages. Drawing and input failures return errors
after terminal restoration.

## Terminal Cleanup

`TerminalGuard` enters raw mode, alternate-screen mode, and hidden-cursor
mode. It restores all three explicitly on normal exit and from `Drop`, so
errors and panic unwinding do not leave the caller's terminal in raw mode.

## Tests

Focused `orynth-tui` tests cover bounded rendering, real run identity, empty
collections, small terminals, UTF-8-safe truncation, projection rendering,
and breakpoint scanning. `orynth` tests cover CLI parsing, deterministic
demo recovery, persisted run listing/selection, SQLite inspection, replay,
fork, diff, and read-only debug behavior. The workspace all-features suite
passed in the Phase F validation run.

## Memory Measurements

Release binary `target/release/orynth.exe`, Windows working set sampled after
the first stable frame with `Get-CimInstance Win32_Process`:

| Scenario | Working set |
|---|---:|
| Empty SQLite database | 6,920 KiB |
| Offline demo, 4 agents | 6,944 KiB |
| Offline demo, 10 agents | 7,064 KiB |

The values are process working-set observations, not a cross-platform RSS
claim. The ten-agent scenario is generated by the same real event-backed demo
builder. The initial frame appeared in the first PTY capture; the current
manual terminal harness has one-second polling resolution, so no sub-second
startup number is claimed.

## Known Limitations

The SQLite source loads the selected stream for recovery before retaining the
bounded display window; the older-page path is bounded but not a streaming
database cursor. Live subscriptions/refresh are manual (`r`), and no remote
provider or plugin process is launched by the demo. Terminal color and
Unicode glyph width are conservative rather than locale-perfect.

## Deferred Features

Safe live runtime controls, provider/plugin process inspection, artifact
browser and retention controls, automatic subscriptions, semantic breakpoint
actions, true cursor-based history streaming, replay/fork UI workflows, and
cross-platform hosted memory measurements remain deferred. These are not
represented as implemented capabilities.
