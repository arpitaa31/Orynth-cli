# Providers

Status: canonical specification derived from the supplied research blueprint.

A provider is a replaceable compute backend. It accepts a typed request and yields typed stream chunks or typed errors, with provider/model identity, capability metadata, cancellation, and usage/cost observations. Unsupported capabilities are explicit; provider-specific features may use extensions.

Providers do not own runs, agents, tasks, permissions, context, budgets, or events. They return observations to the runtime.

Provider usage metadata is authoritative for cache observations. The kernel
`Usage` value carries optional `cached_input_tokens`; providers that cannot
report this value leave it absent. The runtime records observations only when
the field is explicit and rejects values greater than reported input tokens.
The deterministic mock provider intentionally reports no cache metadata.

Phase 1 implements the provider contract, streaming, usage, cancellation, errors, and a deterministic mock. It adds no network adapter or credential requirement.
