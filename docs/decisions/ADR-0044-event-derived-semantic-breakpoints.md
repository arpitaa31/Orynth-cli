# ADR-0044: Event-derived semantic breakpoint scan

Status: accepted

## Context

The debugger needs useful stops such as model changes, invalidation, conflicts,
capability changes, and tool failures. These conditions already exist as typed
runtime events, but the initial inspector is intentionally read-only.

## Decision

`orynth-tui` exposes a pure semantic breakpoint scan over `RecoveredRun`. It
decodes persisted context, assumption, capability, and tool transitions and
tracks model requests in event order. Each hit retains its event sequence,
typed breakpoint kind, and bounded detail. The scanner never pauses a run,
invokes a provider, appends an event, or mutates a projection.

## Consequences

Breakpoint coordinates are grounded in the authoritative event stream and can
be reused by future replay/fork controls. Interactive pause actions, threshold
policies, and live subscriptions remain separate decisions rather than being
smuggled into a read-only renderer.
