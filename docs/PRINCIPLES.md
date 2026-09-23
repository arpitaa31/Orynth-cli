# Principles

Status: canonical specification derived from the supplied research blueprint.

1. Runtime state is authoritative; model output is only a proposal.
2. Agents and models are separate; a logical identity survives model migration.
3. Prefer typed IDs, references, revisions, and events over hidden prompt state.
4. Make deterministic checks before spending model calls.
5. Preserve provider-specific capability metadata instead of flattening semantics.
6. Enforce least privilege for tools, plugins, agents, and protocols.
7. Keep important transitions inspectable and eventually replayable.
8. Bound memory, queues, transcripts, context, and concurrency.
9. Never claim cache hits, rollback, replay determinism, sandboxing, or security without evidence.
10. Keep the kernel small and dependency-light.
11. Classify irreversible effects honestly; compensate only when valid.
12. Keep status, decisions, roadmap, and current work accurate.

