# End Goal

Status: canonical specification derived from the supplied research blueprint.

Orynth is a lightweight Rust runtime for supervised AI agent systems. Logical agents are persistent runtime entities; models are replaceable compute backends. The runtime owns runs, tasks, agents, events, context, artifacts, assumptions, permissions, capabilities, tools, budgets, scheduling, communication, cache metadata, persistence, replay, branches, health, and policy.

Models may reason, propose changes, request tools, publish assumptions, delegate, and recommend scheduling changes. They never own authoritative state. The end state is an inspectable, observable, recoverable agent microkernel, not a chatbot wrapper, prompt collection, workflow graph, or transcript-based group chat.

Delivery is vertical and evidence-based. A feature is complete only when behavior, tests, observability, and documented limitations exist.

