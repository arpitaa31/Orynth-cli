# Phase 08: Debugger and TUI

Status: active; initial projection-backed text inspector implemented.

Build a TUI over runtime projections for agent trees, events, context, assumptions, conflicts, tools, budgets, models, cache observations, permissions, replay, fork, and branch comparison. The initial slice renders a bounded read-only text inspector from `RecoveredRun` and exposes it through `orynth inspect --db <path> --run <id>`. Add interactive controls and semantic breakpoints without creating a second source of truth. Test rendering from deterministic fixtures and terminal-independent state.
