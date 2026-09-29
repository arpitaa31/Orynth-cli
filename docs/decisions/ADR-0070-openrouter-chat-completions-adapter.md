# ADR-0070: OpenRouter Chat Completions at the provider boundary

## Status

Accepted for Phase G; live acceptance remains pending.

## Decision

The first network provider implements the existing `ModelProvider` trait in
`crates/provider`. It uses OpenRouter's Chat Completions endpoint because its
streamed text and function-call format is broadly supported by free and
explicit models. The adapter decodes SSE into Orynth provider events. It never
executes filesystem or process effects.

The key comes only from `OPENROUTER_API_KEY`. Config keeps the route/model ID
per logical agent role and a free-only policy; the resolved model and provider
request ID and selected underlying provider are response observations, not changes of agent identity or route.
The runtime persists those observations in the normal event stream. It also
persists a completion without usage when the provider reports no usage.

The first coding worker sends only Orynth function definitions. OpenRouter
returns a proposal that goes through Orynth's typed tool, capability,
ownership, verification, and event pipeline. No provider-hosted tools or
fallback model list are sent.

## Constraints

The blocking HTTP transport runs on a dedicated thread and sends typed events
through a bounded channel. The provider iterator polls cancellation and enforces
a 90-second total deadline even if a network read stalls; the transport thread
may outlive the iterator until its blocking read returns, but cannot mutate
runtime state. The HTTP client also has per-operation and connection timeouts.
SSE lines, total response size, tool-call count, arguments, and output are
bounded. HTTP error bodies are discarded to avoid secret leakage.
Authenticated requests never follow redirects to another URL.
No automatic retry is performed during free-mode acceptance. Capability
metadata from the provider trait describes the transport; tool support for a
specific model is validated by routing/HTTP response and can still fail.
