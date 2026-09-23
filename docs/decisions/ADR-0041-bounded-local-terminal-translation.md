# ADR-0041: Bounded local terminal translation

Status: accepted

## Context

Phase 7 needs a local command-translation boundary, but arbitrary prose must
not be treated as a reliable command plan. A permissive heuristic translator
would create silent intent expansion at exactly the boundary that controls
filesystem and process effects.

## Decision

`orynth-cli::translate_terminal_text` implements a bounded deterministic grammar
for recognized find, copy, move, remove/delete, process-list, and Git request
phrases. It emits existing `TerminalOperation` values and immediately validates
them with `TerminalPlan`. Unsupported phrases and traversal paths are rejected;
the translator never produces a raw shell command and does not claim model-level
natural-language understanding.

The shell exposes this through `orynth-shell translate`, which renders the same
host-aware plan report as explicit typed requests. Execution still requires the
separate explicit `execute` command and its confirmation boundary.

## Consequences

The project has a useful offline local translation seam without guessing intent
or bypassing policy. A future local/provider-backed model translator can target
the same typed operation contract and retain the current fail-closed behavior.
