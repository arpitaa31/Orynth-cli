# ADR-0046: Line-oriented read-only debug session

Status: accepted

## Context

The first Phase 8 renderer can display a recovered run, but a debugger also
needs event selection and breakpoint navigation. A full-screen terminal
dependency would expand the surface before the state model is settled.

## Decision

`orynth debug --db <path> --run <id>` runs a terminal-independent, line-oriented
session over one recovered projection. It supports `show`, `events`,
`event <sequence>`, `breakpoints`, `pane`, `next`, `prev`, `select`, `help`, and
`quit`. Pane and selection state are terminal-independent and live in
`orynth-tui`. Commands only read the recovered event/projection data and cannot
append events, pause agents, invoke providers, or execute tools.

## Consequences

Event selection and semantic breakpoint output are usable and testable without
terminal-specific state. A full-screen UI, live subscriptions, and mutating
controls can be added later against the same projection boundary.
