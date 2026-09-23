# ADR-0015: Controlled effect fixtures behind tool contracts

Status: accepted.

## Context

The tool pipeline needs evidence for previews, verification, and compensation,
but the core tool-runtime crate must not silently acquire filesystem or process
authority. Tests also need to prove that path traversal, shell-like process
requests, and compensation races fail closed.

## Decision

`orynth-terminal-tools` provides narrow controlled adapters:

- `FilesystemFixture` resolves all paths below a canonical configured root,
  previews without mutation, supports reversible text writes and moves, and
  restores only when the post-effect bytes or move state still match;
- `ProcessFixture<I>` accepts a typed bare program plus indexed arguments and
  delegates invocation to an injected `ProcessInvoker`; shell interpreters and
  shell metacharacters are rejected, and process effects are classified as
  irreversible;
- both adapters implement the existing planner, executor, and verifier
  contracts, so capability authorization and risk/approval remain owned by
  `orynth-tool-runtime`.

These are controlled adapters and test fixtures, not OS-level sandboxes. They
do not claim broad platform enforcement or undoability for irreversible work.

## Alternatives

- Put filesystem/process operations in `orynth-tool-runtime`: rejected because
  it would couple policy contracts to platform effects and weaken testing
  boundaries.
- Execute arbitrary shell strings: rejected because typed process arguments and
  explicit policy are safer and preserve inspectable intent.
- Treat every effect as reversible: rejected because process effects have no
  reliable inverse in this boundary.

## Consequences

The current adapters provide deterministic local evidence for traversal,
preview, verification, compensation, and irreversible-effect policy. Future
work must add context-wide trust propagation and platform-specific isolation
where required.
