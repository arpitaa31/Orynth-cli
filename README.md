# Orynth

Orynth is an experimental AI agent harness/runtime built in Rust.

I started making it because I wanted to explore something beyond the usual:

`prompt → model → response`

The main idea is:

> **Agent != Model**

Models can reason and propose actions, while Orynth manages the actual runtime around them — agents, context, tools, permissions, budgets, communication, persistence and replay.

Basically: **models think, Orynth manages.**

> **Status:** experimental. Most of the runtime works, but the multi-agent/subagent orchestration is still being improved.

---

## What it has

- Chat-first terminal UI
- Live AI Coordinator
- OpenRouter integration
- Streaming responses
- Multi-turn agent execution
- Persistent SQLite runs
- Agent context + typed IPC
- Permissions + resource ownership
- Tool execution + history
- Budgets + health state
- Sandboxed coding workspaces
- Replay / inspect / fork / diff
- Advanced debugger
- Offline demo mode
- Early multi-agent support

---

## Build

You'll need Rust installed.

From the repo root:

```powershell
cargo build --release --workspace
```

The Windows executable will be:
 target\release\orynth.exe

Check available commands:
 .\target\release\orynth.exe --help

# Try it without AI
 No API key needed:
.\target\release\orynth.exe --demo

This opens Orynth's offline demo so you can explore the TUI and runtime.

# Run with OpenRouter
Orynth does not include an API key.
Reviewers/users should create their own OpenRouter key here:
https://openrouter.ai/settings/keys

1. Create your local config

``Copy-Item .\orynth.example.toml .\orynth.toml``

2. Add your OpenRouter key
In the same PowerShell terminal:

``$env:OPENROUTER_API_KEY="YOUR_OPENROUTER_API_KEY"``

Check that it is set:

``if ($env:OPENROUTER_API_KEY) { "KEY SET" } else { "KEY NOT SET" }``

Never put the API key inside the repo or orynth.toml.

3. Test the connection

``.\target\release\orynth.exe provider test openrouter --config .\orynth.toml``

Optional tool-call test:
.\target\release\orynth.exe provider tool-test openrouter --config .\orynth.toml

4. Start a live workspace

Orynth keeps coding work inside its sandbox directory.

``New-Item -ItemType Directory -Force .\sandbox\my-project``

Then:

``.\target\release\orynth.exe --workspace .\sandbox\my-project``

You can now talk to the live Coordinator from the TUI.

# Current limitations
Orynth is still in experimental condition.

The main unfinished part rn is fully reliable multi-agent orchestration. The runtime already has agent, IPC, context, ownership and coordination foundations, but automatic Coordinator → multiple specialist agent delegation is still being improved.
Also, openrouter/free can select different underlying models between requests, so live behavior can vary.
I'd rather ship the actual current state than pretend those parts are finished.

# Why Orynth?
Most AI tools make the model feel like the whole system.
Orynth experiments with the opposite idea:
keep the model replaceable and let the runtime own the important state.
That's what I'm trying to build.

# AI used for this project
Around 20% of the times AI was used, to help me research the architecture, scaffold parts of the Rust workspace, implement, and identify issues.

<img width="1288" height="527" alt="Screenshot 2026-09-29 192922" src="https://github.com/user-attachments/assets/b5b2816a-23cd-41db-b30d-36258fb353b3" />
