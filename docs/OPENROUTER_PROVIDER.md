# OpenRouter provider (Phase G)

Orynth uses its existing `ModelProvider` contract. `OpenRouterProvider` translates
typed `ModelRequest` values to OpenRouter Chat Completions and translates SSE
chunks back to typed `ProviderEvent` values. The adapter owns HTTP only. Orynth
owns agents, context projection, capability checks, tool transactions, event
persistence, budgets, and the Workspace.

Chat Completions was chosen for broad model compatibility and its documented
streaming function-call format. Phase G does not use Responses or any
provider-hosted shell, filesystem, code interpreter, apply-patch, or subagent
tool. Only Orynth-declared `function` tools are sent. OpenRouter returns
proposals; Orynth validates and executes them locally.

## Setup and configuration

Create a key in the [OpenRouter dashboard](https://openrouter.ai/settings/keys).
Set it in the current PowerShell session:

```powershell
$env:OPENROUTER_API_KEY="sk-or-v1-..."
```

Copy `orynth.example.toml` to `orynth.toml`. The example enables
`[providers.openrouter]`, sets the HTTPS base URL
`https://openrouter.ai/api/v1`, references `OPENROUTER_API_KEY`, and assigns
`openrouter/free` to Coordinator and worker. No key value belongs in TOML.
`free_only = true` rejects model IDs other than `openrouter/free` or a slug
ending in `:free`. With `free_only = false`, explicit OpenRouter model slugs are
allowed, but the user must change that setting deliberately. Orynth sends no
fallback `models` list. OpenRouter's free router chooses a compatible free
model per request; the resolved model may vary. `title` optionally sets
`X-Title`; no fake `HTTP-Referer` is sent.

## Stream and usage

The client requests `stream = true` and `stream_options.include_usage = true`.
It emits text deltas as they arrive and assembles indexed function calls from
argument fragments. A stream must end with a finish reason and `[DONE]`.
Malformed, oversized, truncated, or error streams fail closed. The iterator
caps the whole streamed response at the provider contract's 4 MiB payload
limit, including SSE framing and comment lines. It checks cancellation before
dispatch and while waiting for streamed events. A bounded channel separates
the blocking HTTP reader from the provider iterator, which has a 90-second
total deadline and returns promptly on cancellation. A stalled reader thread
may remain until its blocking operation returns, but it cannot mutate Orynth
state after the iterator stops. The HTTP connection timeout is 10 seconds.
HTTP 401/403, 400/422, 404, 408/504, 429, and other errors are
classified without exposing response bodies or the bearer token. There is no
automatic retry, which conserves free-tier requests and avoids duplicate tool
proposals. The 429 error carries a numeric `Retry-After` hint when reported.
The client does not follow HTTP redirects, so an API response cannot redirect
an authenticated request to another host.

The configured route is the model assignment. The actual resolved `model` and
provider request `id`, when present in an SSE chunk, are recorded as model
response metadata events. Orynth opts into `X-OpenRouter-Metadata: enabled`
and records the selected underlying provider when it is reported. The live
Workspace shows the resolved model and provider. Token counts and cached input tokens are recorded only when
the response includes those values. Orynth does not infer monetary cost or a
cache hit. An explicit model that cannot handle a required tool schema fails
with an OpenRouter request error. Requests with tools set
`provider.require_parameters = true` so routing considers the required
parameters. The provider's `capabilities()` describes the transport; it is
not a hardcoded claim that every OpenRouter model supports every feature.
The opt-in `orynth provider tool-test openrouter` command sends one required
function-call probe per distinct Coordinator/worker route. It verifies real
tool-call responses without executing a tool, before the website test.

## Security and data routing

The key is loaded from the environment. It is not logged, persisted as an
event, shown in the debugger, or included in HTTP error text. Coding writes
use the existing rooted `FilesystemFixture` through `ToolRuntime` after schema,
capability, ownership, and policy checks. Live coding workspaces must be
selected below the repository's approved `sandbox` root; the selected
workspace becomes the filesystem tool root.
For the user-requested tiny site only, Orynth's policy approves the existing
confirm-risk write after preview, within a fixed allowlist of `index.html` and
`styles.css`, a 4 KiB per-file limit, and a no-overwrite rule. The model cannot
grant itself a broader capability or approve another tool.

Prompts and projected context sent to OpenRouter leave the local machine and
are processed through OpenRouter and the selected underlying model provider.
Orynth sends bounded recent conversation and bounded context projection, not
the entire repository. Underlying provider privacy, retention, and training
policies can differ. Check OpenRouter's current [privacy
documentation](https://openrouter.ai/docs/guides/privacy/provider-logging) and the
selected provider's policy before sharing sensitive project context.

## Limits

No API key is available in automated tests. The live diagnostic is manual.
`openrouter/free` has changing capacity and rate limits. The current app
supports one Coordinator and one narrow website worker, plus a manual
single-worker `provider site-test openrouter` command. It has no general
autonomous team planner. See [Phase G test procedure](PHASE_G_REAL_MODEL_TEST.md).
