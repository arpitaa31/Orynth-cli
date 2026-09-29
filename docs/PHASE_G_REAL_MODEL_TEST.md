# Phase G real model test

This is an opt-in manual test. Regular `cargo test` remains offline. No key is
stored in the repository or passed to Codex.

From the repository root in PowerShell:

Close any older Orynth session before building so Windows can replace
`target/release/orynth.exe`.

```powershell
Set-Location 'C:\Users\Dell\Desktop\Projects\Orynth'
cargo build --release --workspace
if (-not (Test-Path -LiteralPath .\orynth.toml)) {
    Copy-Item -LiteralPath .\orynth.example.toml -Destination .\orynth.toml
}
$env:OPENROUTER_API_KEY = 'sk-or-v1-...'
.\target\release\orynth.exe provider test openrouter
```

The diagnostic sends one tiny streamed request and expects
`ORYNTH_CONNECTED`. It displays the configured route, resolved model when
reported, token usage when reported, and elapsed time. A missing key produces
a clean error. Do not paste the key into a chat or issue.

Before any coding request, check that the configured Coordinator and worker
routes can return the function calls Orynth needs:

```powershell
.\target\release\orynth.exe provider tool-test openrouter
```

This sends one small request per distinct Coordinator/worker route with a
required function schema and expects `ORYNTH_TOOLS_OK`. The default shared
`openrouter/free` route uses one request. It never executes the proposed
function. If a route cannot return a valid call, stop the coding test and
select a compatible free route or model. The free router may choose a different
underlying model later; each coding request still requires tool parameters.
Both diagnostics are explicit, opt-in requests.

Then launch the live Workspace:

```powershell
.\target\release\orynth.exe
```

Send `Hello`, then `What agents are currently active?` to check real streaming
and grounded runtime context. Exit with Ctrl+C and inspect the recorded run by
using the run ID shown in the debugger:

```powershell
$runId = Read-Host 'Run ID shown in Orynth'
.\target\release\orynth.exe inspect --db .\.orynth\runtime.db --run $runId
```

First test one real coding worker directly, before Coordinator delegation:

```powershell
.\target\release\orynth.exe provider site-test openrouter
```

This command uses one model-powered website worker under a logical
Coordinator parent but does not send a Coordinator model request. It reports
the run ID and workspace. Inspect its Tools and Access history. Once inspected,
clear these two disposable files before the subsequent delegation run:

```powershell
Remove-Item -LiteralPath .\sandbox\phase-g-personal-site\index.html -ErrorAction SilentlyContinue
Remove-Item -LiteralPath .\sandbox\phase-g-personal-site\styles.css -ErrorAction SilentlyContinue
```

For the Coordinator delegation test, start a new live Workspace from the
repository root. The only accepted coding workspace is
`sandbox/phase-g-personal-site`. Enter:

```powershell
.\target\release\orynth.exe
```

```text
Make a simple personal website with the name Arpi and one short paragraph centered on the page.
```

Select the single site worker from AI Team. Its Tools and Access tabs should
show actual proposals, ownership, capability, verification, and commit events.
Inspect `sandbox/phase-g-personal-site/index.html` and optional `styles.css`,
then open `index.html` manually. If the files were created outside Orynth's
tool runtime, the acceptance test fails. If a model does not call tools or
returns invalid content, the attempt fails honestly; retry only after
inspecting the recorded failure and free-tier limits.

After exiting the final live Workspace, use its run ID to verify inspection
and recorded replay without a key or another provider call:

```powershell
$runId = Read-Host 'Final live run ID shown in Orynth'
Remove-Item Env:\OPENROUTER_API_KEY
.\target\release\orynth.exe inspect --db .\.orynth\runtime.db --run $runId
.\target\release\orynth.exe replay --db .\.orynth\runtime.db --run $runId
```

The offline demo requires no key:

```powershell
.\target\release\orynth.exe --demo
```

## Evidence to capture

- The diagnostic response, resolved route, and reported usage.
- Coordinator text arriving incrementally in the LIVE Workspace.
- One logical worker, not an agent per model request.
- Worker tool proposals and committed transaction states in the same run.
- The bounded workspace's actual files and their rendered appearance.
- An offline recorded replay/inspection after exit, without another API call.

No live call has been run by this development session because
`OPENROUTER_API_KEY` is absent. Automated mock-server tests and release build
validation are tracked in `docs/STATUS.md`.
