# TUI

Status: Phase 8 initial slice implemented; canonical specification derived from the supplied research blueprint.

The TUI is a runtime debugger, not merely chat. It will show run/agent trees, roles, models, states, health, progress, context pressure, cache observations, costs, budgets, assumptions, conflicts, blockers, tools, events, artifacts, and permissions.

The current slice is a bounded text inspector. `orynth inspect --db <path>
--run <run-id>` recovers the selected run from the SQLite event store and
renders the persisted runtime, manager, context, assumptions, tools,
capabilities, cache, failure-memory, and recent-event projections. It is read-only: it does
not call a provider, append events, or synthesize a demo run. Its overview
also reports the bounded context freshness dashboard: active tokens and block
lifecycle counts.

The operator app also exposes `orynth replay --db <path> --run <id>
[--at <sequence>]` for recorded prefix recovery, `orynth fork` for explicit
SQLite branch creation and child-prefix materialization, and `orynth diff` for
comparing two recovered projections. Recorded replay and diff never invoke a
provider; fork materialization only copies the validated event prefix and does
not claim to perform live re-execution.

The renderer also scans decoded persisted transitions for semantic breakpoint
hits: model changes, context invalidation, assumption conflicts, capability
grants, approval gates, semantic tool repairs, tool failures, and recorded
failure-memory entries. The scan
returns event sequence coordinates and cannot pause or mutate a run.

`orynth debug --db <path> --run <id>` provides a terminal-independent
interactive session with `show`, `pane`, `next`, `prev`, `select`, `events`,
`event <sequence>`, `breakpoints`, and `quit` commands. Pane state and item
selection live in `orynth-tui`, not in a second persistence model. It is
deliberately read-only; full-screen keyboard navigation and live controls
remain future work.

Future work adds interactive panes and controls for replay, fork, comparison,
and live breakpoint actions covering conflicts, invalidation, high-risk
proposals, escalation, thresholds, promotion, repair, and verification failure.
