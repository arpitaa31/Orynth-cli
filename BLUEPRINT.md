# Orynth Research Blueprint

Source: supplied research brief preserved for future sessions.

You are the principal systems engineer responsible for developing **Orynth**.

This is not a one-shot coding task.

This prompt defines the **LONG-TERM END GOAL** of the project.

Reaching the end goal may require many milestones, many commits, architectural revisions, benchmarks, experiments, and multiple Codex sessions.

Do not attempt to fake completion by implementing shallow versions of everything.

Build Orynth incrementally, keeping the repository in a working and understandable state at every major milestone.

# FIRST ACTION: READ THE REPOSITORY

Before changing implementation code, read:

* `AGENTS.md`
* `BLUEPRINT.md`
* every relevant file under `docs/`
* every relevant file under `plans/`
* current `docs/STATUS.md`
* current `plans/CURRENT.md`

`BLUEPRINT.md` contains the research foundation for this project.

It is not automatically executable requirements.

Your first responsibility is to transform that research into a coherent engineering architecture.

# FIRST PHASE: TURN RESEARCH INTO THE CANONICAL SPECIFICATION

Before implementing the runtime, analyze `BLUEPRINT.md` completely.

Then replace the documentation placeholders with a coherent canonical specification.

At minimum produce and reconcile:

```text
docs/END_GOAL.md
docs/ARCHITECTURE.md
docs/PRINCIPLES.md
docs/SECURITY.md
docs/CONTEXT.md
docs/CACHE.md
docs/AGENTS.md
docs/IPC.md
docs/SCHEDULER.md
docs/TOOLS.md
docs/PROVIDERS.md
docs/PLUGINS.md
docs/PROTOCOLS.md
docs/TERMINAL_AGENT.md
docs/TUI.md
docs/PERSISTENCE.md
docs/TESTING.md
docs/BENCHMARKS.md
docs/ROADMAP.md
```

Then rewrite the files under `plans/` into realistic implementation phases.

Do not blindly copy the blueprint.

Resolve:

* duplicated ideas;
* contradictions;
* MVP versus long-term features;
* kernel primitives versus plugins;
* deterministic mechanisms versus model-driven mechanisms;
* required interfaces;
* dependency direction;
* testing requirements;
* security boundaries;
* performance constraints.

If you materially change a conclusion from the blueprint, document why.

Once the canonical architecture exists, begin implementation.

# ============================================================

# ORYNTH â€” END GOAL

# ============================================================

Orynth should ultimately become:

> A lightweight, Rust-first, provider-independent runtime for supervised AI agent systems in which logical agents are persistent runtime entities, models are replaceable compute backends, context is structured runtime state, tools are capability-controlled transactions, agents communicate through typed IPC, and every important execution transition is observable and recoverable.

Orynth is NOT merely:

* another chatbot library;
* another coding-agent CLI;
* another LangChain clone;
* another static workflow graph framework;
* another wrapper around provider APIs;
* another multi-agent group chat;
* another giant collection of prompts.

The goal is closer to:

> **an agent microkernel/runtime.**

Models perform probabilistic reasoning.

Orynth manages the computational life around that reasoning.

# FUNDAMENTAL INVARIANT

The most important architectural rule is:

> **THE MODEL DOES NOT OWN SYSTEM STATE. THE RUNTIME OWNS SYSTEM STATE.**

Models may reason about state.

Models may propose changes.

Models may request tools.

Models may publish assumptions.

Models may delegate tasks.

Models may recommend scheduling changes.

But authoritative state belongs to Orynth.

Orynth owns:

* runs;
* events;
* agents;
* tasks;
* context;
* artifacts;
* assumptions;
* permissions;
* capabilities;
* tools;
* budgets;
* model assignments;
* scheduling;
* inter-agent communication;
* cache metadata;
* persistence;
* replay;
* branches;
* health;
* execution policy.

# AGENT != MODEL

This must be a first-class architectural distinction.

A logical agent might be:

```text
AUTH-01
Authentication & Session Specialist
```

That identity owns:

```text
mission
task
scope
context references
subscriptions
assumptions
messages
artifacts
permissions
budget
health
progress
history
```

Its current compute backend might initially be:

```text
cheap-fast-model
```

and later become:

```text
strong-reasoning-model
```

without creating a new logical agent.

Orynth must eventually support:

```text
promote(agent, stronger_model)
demote(agent, cheaper_model)
migrate(agent, another_provider)
pause(agent)
resume(agent)
redirect(agent)
fork(agent)
cancel(agent)
transfer_budget(agent_a, agent_b)
```

Model switching must preserve logical agent identity and runtime state.

# SMART MANAGER / ORCHESTRATOR MODE

One major Orynth mode should allow a powerful model to behave primarily as a **manager of other agents**.

The manager should not necessarily perform most implementation work itself.

It should be capable of dynamically deciding:

```text
This task requires:
- authentication expertise
- database expertise
- frontend/design expertise
- testing/security review
```

and creating logical specialists such as:

```text
AUTH-01
DATABASE-02
DESIGN-03
TEST-04
SECURITY-05
```

These identities are runtime roles, not hardcoded personas.

The orchestrator should be able to decide:

* whether a subagent is necessary;
* which specialist identity is appropriate;
* which tools it receives;
* which context it receives;
* what files/resources it owns;
* what model class it requires;
* what budget it receives;
* whether it may spawn children;
* whether it can communicate directly with peers.

A simple task may receive a cheap/local model.

A difficult architectural task may receive a stronger model.

A security-sensitive review may receive a strong read-only model.

The user must also be able to override model selection.

# ACTIVE SUPERVISION

The orchestrator must not simply:

```text
spawn workers
wait
collect answers
```

The long-term system should support active supervision.

The manager should receive structured runtime projections containing information such as:

```text
agent
role
model
task
state
progress
health
budget
context pressure
cache state
recent failures
blockers
assumptions
conflicts
owned resources
recent artifacts
```

The manager should NOT need every worker's complete transcript.

It should receive compact structured deltas and inspect deeper state only when necessary.

The manager should eventually be able to notice:

```text
AUTH-01 has failed the same test three times.
DATABASE-02 changed a schema contract AUTH-01 depends on.
DESIGN-03 has been idle for too long.
TEST-04 discovered a regression affecting AUTH-01.
AUTH-01 is approaching its context limit.
AUTH-01 is cheap but the remaining task is now difficult.
```

and react.

Possible actions:

```text
pause
redirect
ask for status
request explanation
change model
change context projection
grant/revoke capability
transfer budget
spawn reviewer
fork worker
cancel worker
request peer consultation
```

Use deterministic health triggers where possible rather than spending manager-model tokens merely to detect obvious runtime conditions.

# SPECIALIST IDENTITIES

Orynth should support dynamic specialist creation.

Example:

```text
AgentIdentity:
  name: Authentication Specialist
  mission: Own authentication/session implementation
  model_class: CHEAP
  promotable: true

Scope:
  src/auth/**
  src/middleware/auth/**

Subscriptions:
  schema.users.*
  api.auth.*
  security.session-policy

Capabilities:
  filesystem read
  scoped filesystem write
  cargo test
  rg
```

Identities should be useful but lightweight.

Do not turn them into huge persona prompts.

# INTER-AGENT COMMUNICATION

Agents must be able to communicate with:

* the orchestrator;
* their parent;
* children;
* authorized peer agents.

Normal communication should use typed IPC rather than arbitrary copied chat transcripts.

Support concepts equivalent to:

```text
Question
Answer
Assumption
Decision
Conflict
Blocked
Progress
Artifact
ReviewRequest
Warning
Handoff
ContractUpdate
OwnershipRequest
```

Example:

```text
AUTH-01 â†’ DATABASE-02

Question:
subject = schema.users.id

Why:
JWT subject representation depends on this contract.
```

Response:

```text
DATABASE-02 â†’ AUTH-01

Answer:
value = UUID
revision = schema/users@15
evidence = migration artifact
```

Agents should be able to ask each other narrowly scoped questions without absorbing each other's entire contexts.

# ASSUMPTION GRAPH

Cross-agent assumptions should become first-class runtime objects.

Example:

```text
AUTH-01:
users.id = UUID

DATABASE-02:
users.id = BIGINT
```

Orynth should detect normalized deterministic contradictions without requiring an LLM.

The runtime should produce something like:

```text
CONFLICT

AUTH:A17
users.id = UUID

DB:A44
users.id = BIGINT

affected:
JWT subject
refresh-token schema
auth API contract
```

Affected agents should be notified.

Critical agents may be paused until resolution.

Assumptions should support:

```text
identity
owner
subject
normalized value
human-readable claim
evidence
dependencies
revision
confidence
state
```

Semantic/fuzzy conflict detection may later use models, but deterministic conflicts must remain deterministic.

# CONTEXT GRAPH

Do not make:

```rust
Vec<Message>
```

the fundamental state representation.

Context should ultimately consist of typed, versioned, addressable blocks.

Conceptually:

```text
ContextBlock
â”œâ”€â”€ id
â”œâ”€â”€ revision
â”œâ”€â”€ kind
â”œâ”€â”€ owner
â”œâ”€â”€ scope
â”œâ”€â”€ content hash
â”œâ”€â”€ token estimate
â”œâ”€â”€ trust
â”œâ”€â”€ importance
â”œâ”€â”€ state
â”œâ”€â”€ dependencies
â”œâ”€â”€ sources
â”œâ”€â”€ created event
â””â”€â”€ last access
```

Initial scopes should remain simple:

```text
GLOBAL
TEAM
PRIVATE(agent)
```

A model receives a **projection** of runtime context.

The complete context graph may contain far more information than can or should enter a prompt.

# CONTEXT BY REFERENCE

Subagents should receive references whenever practical rather than duplicated giant prompt histories.

Conceptually:

```text
context://schema/users@15
context://project/conventions@4
artifact://migration/users_uuid
```

Agents load or receive projections of what they need.

Do not duplicate hundreds of kilobytes of context merely because another worker spawned.

# CONTEXT DEPENDENCIES

Blocks may depend on other blocks.

Example:

```text
auth-contract@7
    depends on
schema/users@14
```

If:

```text
schema/users@14
```

is superseded by:

```text
schema/users@15
```

Orynth should know that dependent state may be stale.

This should generate explicit runtime state/events.

# CONTEXT SUBSCRIPTIONS

Agents should be able to subscribe to context namespaces/resources.

Example:

```text
AUTH-01 subscribes:
schema.users.*
security.session-policy
api.auth.*

UI-03 subscribes:
api.auth.*
design.tokens.*
```

Changing the database schema should not wake every worker.

Only relevant subscribers should be notified.

# CONTEXT PROPRIOCEPTION

Models should eventually be able to inspect a small context dashboard:

```text
active tokens
budget
largest blocks
pinned blocks
stale blocks
archived/recoverable blocks
recent invalidations
```

Potential tools:

```text
context.inspect
context.search
context.archive
context.restore
context.pin
context.dependencies
```

The model may influence context management.

The runtime remains authoritative.

# CACHE SYSTEM

Prompt caching must be treated as a first-class runtime optimization.

Launching a subagent should NOT unnecessarily destroy useful shared cache locality.

Design prompt rendering around layers such as:

```text
runtime invariants
project identity/conventions
stable tool contract
shared team context
agent identity/permissions
changing context blocks
current task
recent results
```

Push volatility toward the end whenever provider semantics permit it.

Parent and child agents should reuse content-identical stable prefix layers where useful.

Orynth should eventually track:

```text
provider
model
prefix hash
estimated prefix tokens
observed cached tokens
creation time
last hit
expiry hint
routing/provider affinity
```

The scheduler should eventually consider cache affinity when choosing where work runs.

Example:

A slightly more expensive model with a warm 40k-token project prefix may be cheaper overall than migrating to a cheaper cold model.

However:

> semantic cache invalidation is NOT provider KV-cache patching.

Orynth may know that one context block changed.

That does not mean a provider API can replace arbitrary middle tokens in its physical prompt cache.

Keep separate:

```text
semantic dependency state
prompt renderer
provider-specific cache capabilities
```

Never fabricate cache hits.

Use actual provider usage metadata when available.

# EVENT-SOURCED RUNTIME

Important runtime transitions should be immutable events.

Examples:

```text
run.created
user.input
task.created

agent.spawned
agent.paused
agent.resumed
agent.cancelled
agent.health_changed

model.selected
model.requested
model.completed
model.failed
model.switched

context.created
context.attached
context.superseded
context.invalidated
context.archived
context.restored

agent.message

assumption.created
assumption.invalidated
assumption.conflict

tool.proposed
tool.normalized
tool.repaired
policy.checked
tool.approved
tool.executed
tool.verified
tool.compensated
tool.failed

budget.changed
artifact.created

run.completed
```

Materialized runtime state should be reconstructable from authoritative persisted state.

# REPLAY / FORK / TIME TRAVEL

Eventually support:

```text
orynth inspect
orynth replay
orynth fork
orynth diff
```

Distinguish:

```text
REPLAY_RECORDED
REEXECUTE_LIVE
FORK_LIVE
```

Recorded replay uses captured model/tool outputs.

It should not invoke model providers.

Live re-execution may diverge.

Never claim LLM inference is deterministic.

A user should eventually be able to:

```text
inspect event 184
fork before event 184
change model
rerun from there
compare branches
```

# TRANSACTIONAL TOOL RUNTIME

Tool execution must eventually follow approximately:

```text
MODEL PROPOSAL
      â†“
PARSE
      â†“
NORMALIZE
      â†“
SCHEMA VALIDATE
      â†“
DETERMINISTIC REPAIR
      â†“
OPTIONAL MODEL REPAIR
      â†“
CAPABILITY/POLICY CHECK
      â†“
IMPACT ANALYSIS
      â†“
PREVIEW / DRY RUN
      â†“
APPROVAL IF REQUIRED
      â†“
EXECUTE
      â†“
VERIFY
      â†“
COMMIT
      â†“
EVENT
```

On failure, where possible:

```text
COMPENSATE
```

Repair deterministic errors before asking another model.

Possible repair tiers:

```text
A syntax-safe
B schema-safe/unambiguous
C semantic ambiguity
D intent ambiguity
```

Never silently repair dangerous ambiguous intent.

# TOOL EFFECTS

Tools should declare something equivalent to:

```text
REVERSIBLE
COMPENSATABLE
IRREVERSIBLE
```

Examples:

A file edit may preserve its pre-image.

A file move may be moved back.

A Git ref change may record the previous ref.

An already-sent email is not genuinely reversible.

Do not pretend otherwise.

# CAPABILITY SECURITY

Security is a runtime primitive.

Eventually support scoped authority around:

```text
filesystem
process
network
secrets
plugins
external services
```

Example:

```text
filesystem.read:
  ./src/**

filesystem.write:
  ./src/auth/**

process:
  cargo
  git
  rg

network:
  api.github.com
```

A generic:

```text
shell = true
```

is not sufficient as the long-term permission model.

# CAPABILITY LEASES

Future architecture should support temporary capability grants.

Example:

```text
AUTH-01 requests:
filesystem.write(src/middleware/**)

policy:
requires approval

lease:
until task completion
```

Privileges should expire automatically.

# TRUST / PROVENANCE

Context should eventually retain trust/provenance metadata such as:

```text
TRUSTED_PROJECT
USER_PROVIDED
GENERATED
REMOTE_AGENT
WEB_UNTRUSTED
MCP_METADATA
MCP_RESULT
```

This should eventually allow policy such as:

```text
high-impact action
+
material dependence on untrusted content
=
require verification or human approval
```

Do not claim full information-flow security until it actually exists.

# PROVIDER ABSTRACTION

Orynth should support many model providers without collapsing them to the lowest common denominator.

Long-term targets include adapters for or compatibility with:

```text
OpenAI
Anthropic
Gemini
OpenRouter
Azure OpenAI
AWS Bedrock
Groq
Cerebras
xAI
Mistral
Ollama
llama.cpp
vLLM
LM Studio
generic OpenAI-compatible endpoints
future providers
```

Do not necessarily implement every adapter immediately.

The architecture must support capability negotiation for:

```text
streaming
tool calls
parallel tool calls
structured outputs
reasoning controls
vision
audio
context limits
prompt caching
server-side tools
computer use
usage/cost metadata
```

Provider-specific features need escape hatches.

# MODEL SCHEDULER

The scheduler should eventually choose compute based on:

```text
task complexity
required capabilities
latency
cost
context size
historical reliability
tool-call reliability
cache affinity
user preference
privacy/local-only requirements
```

Example:

```text
small local model
â†’ shell translation
â†’ simple classification
â†’ deterministic-repair assistance

cheap cloud model
â†’ routine implementation worker

strong model
â†’ architecture
â†’ difficult debugging
â†’ orchestration
â†’ security review

long-context model
â†’ large-document/repository reasoning
```

The user must always be able to explicitly select or pin models.

# AGENT BUDGETS

Agents should eventually receive explicit budgets:

```text
tokens
money
wall-clock time
tool calls
child agents
context
```

The orchestrator/scheduler should be able to inspect them.

Potential future operation:

```text
transfer_budget(DB-02, AUTH-01, amount)
```

# AGENT HEALTH

Health should become explicit runtime state.

Possible deterministic signals:

```text
repeated identical failure
repeated tool error
no progress
context pressure
budget pressure
assumption conflicts
verification failures
dependency invalidation
```

Example:

```text
AUTH-01

state:
RUNNING

health:
DEGRADED

reason:
cargo test failed three consecutive times
```

This may trigger model promotion or manager intervention.

# PLUGIN SYSTEM

Long-term plugin tiers:

```text
built-in Rust
trusted native extension
isolated process plugin
MCP adapter
WASM/WASI plugin
```

Do not use unstable dynamic Rust ABI loading as the primary extension mechanism.

Process plugins should permit any implementation language.

WASM should eventually support:

```text
capability manifest
memory limits
fuel limits
timeouts
crash isolation
versioning
resource limits
```

WASM belongs after the core runtime is proven.

# MCP

MCP should be a first-class interoperability adapter for tools/resources.

It must NOT define Orynth's kernel abstractions.

Translate:

```text
MCP
â†•
Orynth Tool / Resource primitives
```

Treat MCP server metadata and outputs as potentially untrusted.

Support protocol evolution behind the adapter.

# A2A

A2A should eventually be supported for communication with independent external agents.

Internal agent IPC should remain lighter and strongly typed.

Conceptually:

```text
INTERNAL:
Orynth typed IPC

EXTERNAL:
A2A adapter
```

Do not force network-protocol overhead into internal communication.

# TERMINAL AI

Orynth should eventually ship a flagship terminal assistant.

Example:

```text
$ ai find the 20 biggest files here
```

or:

```text
$ ai clean my Downloads folder
```

A small local model may translate natural language into typed terminal operations.

Prefer:

```text
FindFiles
MoveFile
CopyFile
RemoveFile
ListProcesses
KillProcess
InstallPackage
StartService
GitOperation
RawShellCommand
```

over arbitrary shell strings where practical.

The terminal agent should understand:

```text
OS
distribution
shell
working directory
available commands
permissions
risk
```

Risk classification:

```text
SAFE
CONFIRM
HIGH
BLOCK
```

Example UX:

```text
$ ai clean my Downloads folder

Plan
â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€
Create 4 category folders
Move 73 files
Quarantine 9 duplicates
Permanent deletions: 0

Risk:
LOW

Rollback:
AVAILABLE

Execute? [y/N]
```

Then:

```text
$ ai undo
```

should compensate the transaction when valid.

Do not claim irreversible actions are undoable.

# TUI / AGENT DEBUGGER

Orynth should ultimately have an excellent terminal UI.

This is not merely a chat UI.

It is a runtime debugger.

Show:

```text
run
agent tree
roles
current models
agent states
health
progress
context pressure
cache state
cost
budgets
assumptions
conflicts
blockers
tool transactions
events
artifacts
permissions
```

Example conceptual display:

```text
MANAGER        strong      OK
â”œâ”€â”€ AUTH-01    cheap       DEGRADED
â”œâ”€â”€ DB-02      cheap       OK
â”œâ”€â”€ UI-03      local       WAITING
â””â”€â”€ TEST-04    local       OK
```

Selecting AUTH-01 might show:

```text
role
model
scope
progress
health
context
cache
budget
assumptions
failures
recent events
```

Eventually allow:

```text
inspect
pause
resume
redirect
switch model
inspect context
inspect permissions
replay
fork
compare branches
```

# SEMANTIC BREAKPOINTS

A future debugger should be capable of pausing on conditions such as:

```text
assumption conflict
critical dependency invalidated
high-risk tool proposed
capability escalation
context > threshold
budget > threshold
model promotion
semantic tool repair
verification failure
```

This is more useful for an agent runtime than ordinary line-number breakpoints.

# FAILURE MEMORY

Failed approaches should eventually be representable as first-class runtime information.

Agents should be able to discover:

```text
this approach was already attempted
this command failed for this reason
this migration strategy caused this regression
```

Avoid repeatedly paying models to rediscover the same failed path.

# OBSERVABILITY

Support structured tracing around:

```text
runs
agents
model calls
tool calls
context projections
cache behavior
IPC
assumptions
policy
cost
latency
tokens
failures
```

OpenTelemetry compatibility should eventually be considered.

Do not make telemetry mandatory for local operation.

# PERSISTENCE

The local runtime should remain simple.

SQLite is a strong default for:

```text
events
branches
snapshots
metadata
querying/debugging
```

Large immutable payloads/artifacts/context content may live in a content-addressed blob store.

Keep storage interfaces abstract enough to evolve.

# PERFORMANCE

Orynth should remain unusually lightweight.

The harness/runtime target excludes local LLM processes.

Long-term target:

```text
CLI idle                         <25 MB RSS desirable
TUI idle                         <35 MB desirable
one normal remote agent          <50 MB desirable
four logical agents              <75 MB desirable
ten mostly waiting agents       <100 MB desirable
```

The fundamental target remains:

> **approximately <100 MB harness RAM for realistic remote-model workloads, excluding local model processes and clearly separated external plugin processes.**

Measure this.

Do not merely claim it.

Logical agents should mostly be:

```text
IDs
metadata
references
queues
counters
subscriptions
```

NOT:

```text
dedicated OS thread
copied repository
copied transcript
embedded model
huge independent context
```

# CROSS-PLATFORM

Long-term target:

```text
Linux
macOS
Windows
```

Avoid architecture that unnecessarily assumes one shell or filesystem model.

Platform-specific terminal behavior should live behind adapters.

# SYSTEM PROMPT

Orynth should not require an enormous monolithic system prompt simply because it has many capabilities.

Use:

```text
small runtime invariants
+
dynamic capability exposure
+
progressive tool discovery
+
context projection
+
agent-specific role/task
```

rather than dumping every tool/plugin/agent description into every prompt.

# TOOL DISCOVERY

Large tool ecosystems should support progressive discovery.

Do not place hundreds of MCP/plugin tools into every model request.

Eventually support:

```text
tool namespaces
tool search
capability manifests
dynamic exposure
task-relevant tool sets
```

# USER CONTROL

The runtime should remain inspectable.

Users should be able to configure:

```text
models
providers
manager model
worker model
local models
budgets
parallelism
permissions
tool policy
plugins
cache preference
agent overrides
context limits
approval policy
```

Do not make automatic routing impossible to override.

# CONFIGURATION

Aim toward a readable TOML configuration such as:

```toml
[runtime]
max_concurrent_agents = 6
event_store = ".orynth/runtime.db"
artifact_dir = ".orynth/artifacts"

[models.manager]
provider = "..."
model = "..."
class = "strong"

[models.worker]
provider = "..."
model = "..."
class = "cheap"

[models.local]
provider = "openai-compatible"
base_url = "http://127.0.0.1:11434/v1"
model = "..."
class = "local"

[scheduler]
mode = "economy"
prefer_warm_cache = true
promote_after_failures = 3

[context]
semantic_invalidation = true
subscriptions = true

[security]
default_network = "deny"

[plugins]
process = true
mcp = true
wasm = true
```

The final exact schema should evolve from implementation evidence.

# ============================================================

# DEVELOPMENT STRATEGY

# ============================================================

The END GOAL above is a destination.

It is NOT permission to build everything simultaneously.

Do not produce 100 shallow modules containing TODOs.

Build vertical slices.

Every major phase must leave Orynth genuinely usable or demonstrably closer to the architecture.

# PHASE 1 â€” FOUNDATION

Establish:

```text
core IDs/types
Run
Task
Event
Agent identity
provider interface
mock provider
basic model loop
streaming
usage
cancellation
configuration
```

# PHASE 2 â€” RUNTIME CORE

Establish:

```text
event store
state reconstruction
artifacts
snapshots
recorded replay
fork metadata
tool definitions
basic policy
```

# PHASE 3 â€” CONTEXT

Establish:

```text
typed context blocks
content addressing
revisions
dependencies
scopes
projections
subscriptions
invalidation
prompt renderer
prefix hashing
cache telemetry
```

# PHASE 4 â€” MULTI-AGENT

Establish:

```text
logical subagents
stable identities
typed IPC
assumptions
conflicts
ownership
budgets
agent health
manager projections
cheap/strong model classes
model promotion
```

# PHASE 5 â€” TOOLS + SECURITY

Establish:

```text
transactional tool pipeline
schema validation
deterministic repair
risk classification
capabilities
approvals
verification
compensation
filesystem/process policy
trust metadata
```

# PHASE 6 â€” EXTENSIONS + PROTOCOLS

After the kernel is proven:

```text
process plugins
MCP
plugin discovery
tool discovery
WASM/WASI
A2A
capability leases
secret handles
```

Do not force all of these into one release.

# PHASE 7 â€” TERMINAL AI

Build:

```text
local command translator
typed terminal operations
risk classifier
preview
confirmation
execution
verification
compensation
OS/shell detection
```

Make this one of Orynth's flagship demonstrations.

# PHASE 8 â€” TUI / DEBUGGER

Build the runtime inspector.

Focus on:

```text
agent tree
events
context
assumptions
conflicts
tools
budgets
models
cache
permissions
fork/replay
```

# PHASE 9 â€” ADVANCED ORCHESTRATION

After the fundamentals are reliable:

```text
dynamic specialist creation
active supervision
automatic promotion/demotion
peer consultation
speculative workers
shadow verification
budget transfers
advanced cache-aware scheduling
failure memory
semantic breakpoints
context freshness policies
```

# PHASE 10 â€” HARDENING

Focus on:

```text
security testing
fuzzing
provider failures
crash recovery
plugin failures
SQLite recovery
cross-platform behavior
benchmarks
RSS
startup
documentation
API stability
release packaging
```

# ============================================================

# WORKING RULES FOR CODEX

# ============================================================

Do not attempt to finish the entire END GOAL in a single uncontrolled coding pass.

At the beginning of each major development phase:

1. inspect current repository state;
2. read `docs/STATUS.md`;
3. read `plans/CURRENT.md`;
4. read relevant canonical docs;
5. inspect existing implementation;
6. define the smallest coherent milestone;
7. define acceptance tests;
8. implement it;
9. test it;
10. update documentation;
11. continue.

# PERSISTENT PROJECT MEMORY

This project may outlive this Codex session.

Therefore the repository itself must always explain:

```text
what exists
what works
what is incomplete
what was decided
what is currently being implemented
what should happen next
```

Keep:

```text
docs/STATUS.md
docs/ROADMAP.md
plans/CURRENT.md
docs/decisions/
```

accurate.

At the end of any substantial work session, update them.

A future Codex session should be able to continue without relying on hidden conversation history.

# ARCHITECTURE DECISIONS

For important decisions create ADRs under:

```text
docs/decisions/
```

Record:

```text
context
decision
alternatives
consequences
status
```

Do not create ADRs for trivial code choices.

# TESTING

Testing is part of the architecture.

Eventually cover:

```text
event reconstruction
recorded replay
fork isolation
context invalidation
context privacy
assumption conflict
agent model migration
budget exhaustion
tool repair
policy denial
filesystem compensation
provider timeout
malformed provider output
plugin crash
SQLite recovery
cancellation
cache telemetry
```

Use deterministic mock providers heavily.

Core tests must not require paid API credentials.

# VALIDATION

At appropriate boundaries run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Before releases:

```bash
cargo build --release --workspace
```

Fix failures.

Do not silence tests merely to obtain a green build.

# DEPENDENCIES

Prefer mature and focused Rust dependencies.

Likely useful technologies include:

```text
tokio
reqwest + rustls
serde
serde_json
toml
schemars
jsonschema
rusqlite
tracing
tracing-subscriber
blake3
ulid or UUIDv7
ratatui
crossterm
globset
thiserror
```

These are guidance, not mandatory choices.

Do not add large dependencies without need.

Feature-gate heavyweight optional systems.

# MEMORY DISCIPLINE

Avoid:

```text
full transcript copies
full context copies
per-agent repository copies
per-agent OS threads
unbounded queues
unbounded tool outputs
unbounded in-memory event history
```

Use:

```text
IDs
references
immutable blobs
bounded channels
lazy loading
archival
snapshots
streaming
```

# SECURITY DISCIPLINE

Treat:

```text
model output
web content
MCP metadata
MCP output
plugin output
remote-agent messages
generated code
```

as potentially untrusted.

The model cannot grant itself capabilities.

Tool schema validity does not imply permission.

Do not log secrets unnecessarily.

Do not permit path traversal.

Do not silently broaden scopes.

# NO FAKE FEATURES

Never implement a CLI command that merely prints what it would eventually do while presenting itself as implemented.

Never mark a roadmap feature complete because interfaces exist.

Never claim:

```text
rollback
sandboxing
cache hit
replay
security
provider support
cross-platform support
```

unless the corresponding behavior exists and is tested to an appropriate degree.

# SIMPLICITY RULE

When choosing between:

```text
clever architecture
```

and:

```text
small explicit architecture satisfying the invariant
```

prefer the latter.

Orynth's advantage should come from powerful primitives, not abstraction volume.

# SELF-REVIEW

Periodically inspect Orynth as if you were an external systems engineer.

Ask:

```text
Has Agent accidentally become Model?
Has Context become Vec<Message>?
Has Event Store become mere logging?
Has the orchestrator become an enormous chat transcript?
Are tools bypassing policy?
Are plugins bypassing capabilities?
Are subagents copying huge contexts?
Are provider abstractions losing important capabilities?
Are cache claims evidence-based?
Are irreversible actions presented as reversible?
Are we adding framework abstractions instead of runtime primitives?
Is memory use growing unnecessarily?
```

Correct architectural drift when discovered.

# FLAGSHIP LONG-TERM DEMONSTRATION

Orynth should eventually be able to demonstrate something like:

```text
$ orynth run "add secure authentication to this project"

MANAGER
strong-model

spawning specialists...

AUTH-01
Authentication Specialist
model: cheap-worker

DB-02
Database Specialist
model: cheap-worker

SEC-03
Security Reviewer
model: strong-reviewer
permissions: read-only
```

During execution:

```text
DB-02 updates:
schema.users.id
BIGINT â†’ UUID
```

Runtime:

```text
dependency invalidated

affected:
AUTH-01
API-04

AUTH-01 context marked stale
```

Then:

```text
ASSUMPTION CONFLICT

AUTH:A17
users.id = BIGINT

DB:A44
users.id = UUID
```

The manager receives the conflict.

DB answers AUTH through typed IPC.

AUTH continues.

Later:

```text
AUTH-01
cargo test failed 3 times

health:
DEGRADED
```

Scheduler/manager performs:

```text
model switch

AUTH-01:
cheap-worker â†’ strong-reasoner
```

The `AgentId` remains identical.

The stronger model fixes the problem.

Security reviewer verifies the change.

The user opens the TUI and sees:

```text
MANAGER       strong      OK
â”œâ”€â”€ AUTH-01   strong      OK
â”œâ”€â”€ DB-02     cheap       DONE
â””â”€â”€ SEC-03    strong      REVIEW
```

The user selects the earlier conflict event.

They can inspect:

```text
context
assumptions
messages
tool transactions
model
budget
cache
permissions
```

They fork the execution before the bad decision.

The alternate branch uses another model.

The two branches can be compared.

That demonstration should emerge from genuine runtime primitives, not hardcoded demo logic.

# SECOND FLAGSHIP DEMONSTRATION

Terminal AI:

```text
$ orynth-shell clean up my downloads folder

Analyzing...

Proposed transaction

Create:
  Downloads/Documents
  Downloads/Images
  Downloads/Archives

Move:
  73 files

Quarantine:
  9 duplicate files

Permanent deletion:
  0

Risk:
  LOW

Reversible:
  YES

Execute? [y/N]
```

After execution:

```text
$ orynth-shell undo

Compensating transaction...

82 / 82 operations restored.
```

Again: only claim reversibility when the tool actually supports it.

# END STATE

Orynth's eventual identity should be obvious:

> **Orynth is the runtime underneath agent systems, not merely another agent sitting inside them.**

Its strongest ideas should reinforce each other:

```text
logical agents
        +
replaceable models
        +
active supervision
        +
typed IPC
        +
assumption graph
        +
versioned context graph
        +
cache-aware rendering
        +
event sourcing
        +
transactional tools
        +
capability security
        +
replay/fork
        +
runtime debugger
```

The important differentiation is not the number of features.

It is that all of these features operate on the same authoritative runtime model.

# BEGIN

Start by reading `BLUEPRINT.md` completely.

Then inspect the repository.

Your first substantial task is NOT writing large amounts of Rust.

First transform the blueprint into the canonical Orynth engineering specification and long-term roadmap.

Review that architecture for contradictions and unnecessary complexity.

Then identify the smallest coherent implementation milestone.

Update:

```text
docs/STATUS.md
docs/ROADMAP.md
plans/CURRENT.md
```

and begin implementation.

Continue milestone by milestone.

Do not treat the END GOAL as something that must fit into one session.

Do not lower the END GOAL merely because it will take a long time.

Build toward it correctly.
