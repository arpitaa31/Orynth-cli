# ADR-0037: Opaque Secret Handles

Status: accepted

## Context

Secret capability authority is already represented by scoped, expiring leases,
but passing secret bytes through model prompts, plugin manifests, event
payloads, or debug output would create unnecessary disclosure paths. External
adapters need a host-owned reference that can be resolved only in the
authorized agent/task context.

## Decision

`orynth-security::SecretVault` stores bounded secret bytes in memory and
returns an opaque `SecretHandle`. A handle is bound to its issuing agent and
optional task. Both issuing and resolving a handle require a current
`CapabilityDomain::Secrets` lease for the requested resource. Handles can be
revoked independently. Secret values are not part of capability transitions,
and their debug representation is redacted.

The vault is intentionally not an event-store or persistence boundary. Hosts
must inject a replacement secret source when a process, plugin, or runtime
lifecycle requires durable external secret management.

## Consequences

Adapters can pass references rather than secret material across internal
planning boundaries while retaining last-moment lease checks. Secret handles
are bounded and testable, but this slice does not provide a platform keyring,
encryption-at-rest, or automatic secret rotation.
