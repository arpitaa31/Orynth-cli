# ADR-0001: Runtime authority and foundation boundaries

Status: accepted

## Context

The research blueprint distinguishes logical agents from models and requires authoritative runtime state, observable transitions, provider independence, bounded resources, and incremental vertical delivery. The bootstrap repository had no implementation or canonical boundary.

## Decision

Keep the kernel provider-independent and dependency-light. Model providers return typed observations; they do not own runs, tasks, agents, context, permissions, budgets, or events. Phase 1 implements only an in-memory foundation slice: IDs and lifecycle values, a trace, streaming provider contracts, usage, cancellation, a deterministic mock, a basic agent loop, and configuration.

Persistence, context, tools, security, multi-agent transport, plugins, protocol adapters, terminal operations, and TUI remain separate later phases.

## Alternatives

- Put provider calls and state in the application: rejected because model state would become authoritative and hard to replay.
- Build all long-term subsystems at once: rejected because shallow contracts would falsely imply completed behavior.
- Make the kernel depend on async/network/database frameworks: rejected because it would make the authority boundary heavy and provider-specific.

## Consequences

The first slice is intentionally modest but testable. Later services can consume stable primitives without forcing provider or UI choices into the kernel. Some desired behavior is unavailable until its documented phase and must not be advertised as implemented.

