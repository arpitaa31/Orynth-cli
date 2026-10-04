# Reviewer feedback release patch acceptance

Status: pending live retest. Do not mark the patch release ready from offline
tests alone.

## Test A: TOML key

Copy `orynth.example.toml` to the gitignored `orynth.toml`. Set a local
`[providers.openrouter] api_key` and run `orynth provider test openrouter
--config ./orynth.toml` with `OPENROUTER_API_KEY` unset. Confirm a connection
and that the key appears in no output, event, or debugger view. In this
development session the environment variable is absent and no local test key
was supplied, so a live OpenRouter connection has not been verified. Offline
provider tests cover direct-key precedence, named environment fallback, and
missing-key errors.

## Test B: model config

Set an explicit valid OpenRouter model slug under `[models.coordinator]` with
`free_only = false` unless the slug ends in `:free`. Launch `orynth --config
./orynth.toml`. Confirm the header shows the selected config, the exact
configured slug, and the model/provider reported by OpenRouter. Repeat with
`openrouter/free` and confirm a different resolved model is labelled as
resolution. Live verification is pending; offline header and config tests were
added.

## Test C: general subagent

Launch with a fresh directory below `sandbox/`. Prompt: "Build a browser-based
VS Code-like editor with a file sidebar, editor pane, tabs, and dark styling.
Use at least one subagent for implementation work." Confirm AI TEAM contains
the real child AgentId, `AgentCreated` and task IPC are in the run, and files
were written and verified by Orynth's rooted filesystem tool. Live verification
is pending; the generic four-file editor worker and scope tests were added.

## Test D: interrupt

During a multi-step Coordinator task, press Esc while streaming. Confirm the
task stops, the Workspace stays open, a cancellation event is recorded, and
input accepts "Actually stop that. Just create a README instead." Confirm the
same Coordinator session continues. Also verify Ctrl+C stops a busy turn and
quits when idle. Live verification is pending; focused cancellation and TUI
tests were added.

## Local validation

`cargo fmt --all -- --check` passes. The exact required commands now pass:

| Command | Local result |
| --- | --- |
| `cargo check --workspace --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --workspace --all-features` | Passed, including app 47/47, provider 27/27, TUI 54/54, integration tests, and doc tests |
| `cargo build --release --workspace` | Passed; the rebuilt `target/release/orynth.exe` differs from the pre-patch binary and runs `help` with `--config` shown |

Earlier attempts were intermittently blocked by Windows Application Control
(OS error 4551) and one isolated release link ran out of disk space, but those
host failures did not recur on the passing exact commands. The focused CLI,
provider, app, and TUI suites also passed 20/20, 27/27, 47/47, and 54/54.
The release binary rejects an explicit example config without credentials with
the clear setup error `OpenRouter credentials are missing: set api_key or
api_key_env`.
`OPENROUTER_API_KEY` is absent, and the local `orynth.toml` has no direct key,
so manual A-D could not be run against OpenRouter.
