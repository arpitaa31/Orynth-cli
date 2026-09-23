# Phase 07: Terminal AI

Status: active, typed planning boundary implemented.

The first Phase 7 slice adds `TerminalEnvironment` host observations and a
bounded `TerminalPlan` over typed operations. It discovers OS, architecture,
shell, working directory, permission hints, and available commands; classifies
Safe/Confirm/High/Block risk; requires explicit confirmation for effects; and
rejects path escapes and unknown Git actions. The rooted filesystem adapter now
executes bounded find/copy/quarantine-backed remove operations with verification
and conflict-checked compensation. A bounded local phrase grammar translates
recognized find/copy/move/remove/process-list/Git-read requests into typed
operations and rejects unsupported prose. `orynth-shell plan` parses the typed
requests and renders a deterministic report; explicitly confirmed
`orynth-shell execute` commands run the rooted filesystem subset through the
tool runtime, including preview, execute, verify, and commit. Rooted
copy/move/quarantine effects persist bounded relative compensation records, and
`orynth-shell undo` restores the latest record after rechecking conflicts. The
full model-backed natural-language translator, process/Git adapters, stronger
crash semantics, and cross-platform terminal UX remain later work.
