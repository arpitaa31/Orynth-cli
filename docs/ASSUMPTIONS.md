# Assumptions

Status: Phase 4B deterministic assumption/conflict slice implemented.

An assumption is a runtime-owned claim with an `AssumptionId`, run and owner
identity, normalized subject/value, human-readable claim, evidence references,
dependencies, revision, confidence, lifecycle state, and trust origin. Models
may propose assumptions; they do not make them authoritative.

`orynth-assumptions` normalizes subjects and values by trimming, collapsing
whitespace, and lowercasing. Publishing a claim with the same normalized
subject and a different normalized value creates a deterministic
`AssumptionConflict`, marks both assumptions `Conflicted`, and reports the
affected owners in stable order. Equal normalized values do not conflict.
Assumptions retain a direct trust origin plus bounded material-input origins.
Publication combines those origins without upgrading trust, and replay rejects
persisted claims that are more trusted than their recorded inputs. Legacy
version-1 transitions decode as generated assumptions with no input-origin
list.

Creation, state changes, and conflict detection are versioned transitions.
`RuntimeService::publish_assumption` checks run/owner membership, stages the
graph, builds runtime-originated `Conflict` IPC notifications for every
affected owner, checks all mailbox capacities, and atomically appends the
assumption transitions plus notifications. If any recipient is full, neither
the assumption nor its notifications are appended. The event stores preserve
the opaque payloads; runtime recovery replays them. Fork materialization
remaps the run ID embedded in created assumptions and notifications.

This slice does not claim fuzzy semantic contradiction detection, authorization
of claims, automatic agent pausing, or delivery acknowledgements. Those require
policy and supervision work built on this deterministic base.
