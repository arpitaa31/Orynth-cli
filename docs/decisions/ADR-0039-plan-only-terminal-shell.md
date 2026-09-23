# ADR-0039: Plan-only terminal shell boundary

Status: accepted

## Context

The typed terminal planner is useful only if a caller can inspect its result,
but a first CLI must not silently turn typed or model-provided text into an
ambient shell. The initial shell boundary therefore needs bounded parsing,
host observations, deterministic rendering, and an explicit no-effect promise.

## Decision

`orynth-cli` parses only explicit `plan` commands for the supported typed
terminal operations. It bounds argument count and text size, rejects unknown or
incomplete flags, delegates operation validation to `TerminalPlan`, and renders
the result with `TerminalEnvironment` observations. `apps/orynth-shell` exposes
this as a plan-only executable and returns a nonzero exit code for malformed
requests or environment/reporting failures.

The `shell` operation remains representable so the planner can classify it as
blocked, but neither the CLI nor the app executes it. Filesystem, process, Git,
and future natural-language effects remain behind capability-checked adapters
and explicit runtime integration.

## Consequences

The project has a usable inspection/UX boundary without claiming a terminal
assistant, shell sandbox, or effect execution path. The parser and report can be
tested offline and can later be embedded by a richer assistant without changing
the typed planner contract.
