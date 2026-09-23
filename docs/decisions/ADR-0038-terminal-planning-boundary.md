# ADR-0038: Typed Terminal Planning Boundary

Status: accepted

## Context

The terminal assistant must not turn natural-language or model output into an
ambient shell with implicit authority. It needs a deterministic boundary that
can report host facts, classify risk, require confirmation, and distinguish
reversible work from irreversible effects before any platform adapter runs.

## Decision

`orynth-terminal-tools` provides bounded `TerminalEnvironment` observations and
`TerminalPlan` over typed operations: file discovery, move/copy/remove,
process listing, allowlisted Git actions, and an explicitly blocked raw-shell
escape hatch. Plans classify the highest operation risk as Safe, Confirm, High,
or Block. Confirm and High plans require explicit confirmation; Block plans
cannot be authorized. Relative path escapes and unknown Git actions are
rejected. Compensation is reported only for typed move/copy operations and
quarantine-backed remove operations.

The planner reports permission hints and available commands as observations,
not authority. Concrete execution remains behind capability-checked platform
adapters. Natural-language translation, additional platform adapters, and the
production terminal UX are separate later work; the rooted fixture already
provides the bounded quarantine path used by remove execution.

## Consequences

The assistant can render an honest, testable plan before execution without
silently broadening process or filesystem scope. The current slice is useful
for policy and UX integration but deliberately does not claim a complete
terminal assistant or OS sandbox.
