# ADR-0042: Persistent rooted terminal undo

Status: accepted

## Context

In-process compensation is insufficient for a shell command: the user must be
able to invoke `undo` in a later process, while the runtime must not blindly
restore a path that another actor changed. Persisting complete transaction state
would also expose more authority and data than this first terminal slice needs.

## Decision

The CLI persists at most 32 relative filesystem compensation records in
`.orynth/terminal-undo.log` below the detected working-directory root. Records
are hex-encoded and bounded, and the journal uses a synced temporary file plus
replace/backup recovery handling. Copy records retain a content fingerprint and
length; move and quarantine records retain only relative paths.

`orynth-shell undo` loads the latest record, rechecks a current filesystem
capability, and asks `FilesystemFixture` to compensate only if the expected
post-effect state is still present. Conflicts leave the record available for
inspection/retry. Successful undo removes only that latest record.

The journal is deliberately not a claim of crash-atomic coupling between the
effect and journal write; the effect/journal crash window and durable event
integration remain hardening work.

## Consequences

The flagship copy/move/quarantine flow now has genuine cross-process undo with
conflict protection and bounded storage. The implementation does not yet
provide persistent undo for process/Git effects, arbitrary directory cleanup,
or a full event-sourced terminal transaction history.
