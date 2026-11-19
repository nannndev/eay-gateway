# HTTP API

Base URL: `http://127.0.0.1:20129`. All bodies are JSON. Errors look like
`{"detail":"…","error":{"message":"…"}}`, or OpenAI-style `{"error":{"message":"…","type":"gateway_error"}}`
on `/v1`.

## OpenAI-compatible API

### `POST /v1/chat/completions`

OpenAI Chat Completions request: `model`, `messages` (text, `image_url` / `input_image`, `file` / `input_file` PDFs & documents, `video_url`, `input_audio`, tool calls, tool results),
`tools`, `temperature`, `max_tokens`, `stream`. See [Routing](routing.md) for `model`.

- `stream: true`: server-sent `chat.completion.chunk` events ending with `data: [DONE]`
- `stream: false`: a single `chat.completion` with `usage`

Responses come back in OpenAI format whatever the upstream speaks. When every attempt fails the
status is **502** with the last upstream error in the message.

Also served at `/gateway/v1/chat/completions`.

#### Thinking and reasoning effort

Ask for more reasoning with `reasoning_effort` (`minimal`, `low`, `medium`, `high`), Anthropic's
`thinking`, or Responses' `reasoning.effort`. The gateway translates it for whichever provider
answers:

| Upstream | Sent as |
|---|---|
| Claude (subscription or API key) | `thinking` with a token budget (`low` ≈ 4k, `medium` ≈ 10k, `high` ≈ 32k); `max_tokens` is raised above the budget and `temperature` is dropped, as Anthropic requires |
| ChatGPT / Codex | `reasoning.effort`, with reasoning summaries |
| Gemini | `thinkingConfig.thinkingBudget` |
| OpenAI-compatible APIs | `reasoning_effort`, unchanged |

The model's reasoning comes back as `reasoning_content` in chat chunks and as `thinking` blocks to
Anthropic clients. Claude's signed thinking is passed back to Claude with tool results, as Claude
requires for tool use with thinking. Session memory stores answers, not reasoning.

### `POST /v1/messages`

Anthropic-native Messages endpoint (used by Claude Code CLI and Anthropic SDKs). Translates requests (`system`, `messages` with text/images/PDFs/`tool_use`/`tool_result`, `tools`, `stream`) and returns Anthropic-format JSON or SSE (`message_start`, `content_block_delta`, `message_stop`). Bare Claude model ids automatically resolve to your connected `claude` OAuth or `anthropic` provider (or fall back to `auto`). `thinking` (Claude Code's "think hard") works with every provider; see [Thinking and reasoning effort](#thinking-and-reasoning-effort). Also served at `/gateway/v1/messages`.

### `POST /v1/responses`

OpenAI Responses endpoint (`instructions`, `input`, `max_output_tokens`, `temperature`, `reasoning.effort`, function `tools`, `stream`). Translates requests and returns `response` objects or SSE. Also served at `/gateway/v1/responses`.

### `POST /v1/embeddings`

Forwards OpenAI-compatible embedding requests (`model`, `input`, …) to connected OpenAI-family providers. Also served at `/gateway/v1/embeddings`.

### `POST /v1/images/generations`

Forwards OpenAI-compatible image generation requests (`model`, `prompt`, `size`, `n`, …) to connected OpenAI-family providers with virtual-key auth, failover, and cooldowns. Also served at `/gateway/v1/images/generations`.

### `POST /v1/audio/speech`

Forwards OpenAI-compatible Text-to-Speech requests (`model`, `input`, `voice`, `response_format`, …) and returns the upstream audio stream/bytes. Also served at `/gateway/v1/audio/speech`.

### `POST /v1/audio/transcriptions` & `POST /v1/audio/translations`

Forwards `multipart/form-data` audio uploads (`file`, `model`, …) to connected OpenAI-compatible providers. The `model` can be specified in the multipart form or via `?model=` query parameter. Also served under `/gateway/v1/audio/*`.

### Request Headers (Skills, Memory & Guardrails)

| Header | Description |
|---|---|
| `x-eay-skills` | Comma-separated skill names to inject into the system prompt (alternative to `model+skill` suffix) |
| `x-eay-session-id` | Works with streaming and non-streaming requests. Persist and replay conversation history for this session id in SQLite. Sessions are private to the caller: each virtual key (and the no-key local caller) has its own namespace, so the same id under two keys is two different sessions |
| `x-eay-team` | Use this roster. Send the team id (`tm_…`), or its name when exactly one team has that name. The speaker must be one of its agents (`model: agent/<lead>`), and `eay_delegate` may name only the other members. Sent with a session id, the session stores the team id. Renaming the team does not drop the chat. A new session id starts an empty chat on the same roster. Two sessions may use one roster and do not share messages |
| `x-eay-run-id` | Your own id for this request (≤64 chars, `[A-Za-z0-9_-]`), so you can follow it on `/gateway/admin/events` |
| `x-eay-max-context-chars` | Trim older non-system messages when total characters exceed this limit |
| `x-eay-redact` | Set to `true` or `1` to redact common API keys/tokens in user messages before forwarding upstream |
| `x-eay-mcp` | Comma-separated external MCP server names (or `*` for all enabled) to attach and execute MCP tools automatically |

### `GET /v1/models`

`{"object":"list","data":[{"id":"auto","object":"model","owned_by":"eay-gateway"}, …]}`. Auto modes
come first, then the models of connected providers (`owned_by` = provider id).
Also at `/gateway/v1/models`.

### Virtual keys

Without an `Authorization` header (or with any key not starting with `gw-sk-`) requests are
accepted, as they come from this machine. A `gw-sk-…` key must exist and be enabled (otherwise
**401**), and its limits apply:

- `allowed_providers`: provider ids or aliases it may use; anything else is **403**
- `rate_limit_per_min`: requests per minute; over it is **429**
- `max_tokens_budget`: total token cap (`input + output`); over it is **429**
- `max_cost_usd`: total estimated USD cost cap; over it is **429**
- `default_skills`: list of skill names automatically injected for requests using this key

| | |
|---|---|
| `GET /gateway/admin/keys` | List keys (prefix only, plus usage totals and limits) |
| `POST /gateway/admin/keys` | `{"name":"ci","allowed_providers":["deepseek"],"rate_limit_per_min":30,"max_tokens_budget":100000,"max_cost_usd":5.0,"max_concurrent":4,"default_skills":["rust"],"tool_policy":{"mode":"read_only"}}` → returns the secret `key` once. `tool_policy` limits gateway-run MCP tools (see [Tool policies](#tool-policies)). `rate_limit_per_min` counts client requests (failover attempts and MCP tool rounds inside one request do not count extra). `max_concurrent` caps requests in flight at once (a streamed response holds its slot until it ends); over the cap → `429` |
| `PATCH /gateway/admin/keys/:id/toggle` | Enable / disable |
| `DELETE /gateway/admin/keys/:id` | Delete |

## Health

| | |
|---|---|
| `GET /health` | `{"service":"eay-gateway","status":"ok","version":"0.1.0"}`; probe this before starting another gateway |
| `GET /gateway/health` | `{"status":"ok"}` |

## Providers (API keys)

| | |
|---|---|
| `GET /gateway/admin/providers` | Every API-key provider: `connected`, `enabled`, `base_url`, `models`, `custom_models`, `seed_models`, `synced_count`, `models_synced_at`, `extra_keys`, `cooling`, `latency_ms`, `error` |
| `POST /gateway/admin/providers/:id/connect` | `{"api_key":"…","base_url":"…"}`: verifies, saves, syncs models |
| `POST /gateway/admin/providers/:id/disconnect` | Forget the key and its synced models |
| `PATCH /gateway/admin/providers/:id/toggle` | `{"enabled":false}`; keeps the key |
| `POST /gateway/admin/providers/:id/keys` | `{"api_key":"…","label":"…"}`: add another key (verified first) |
| `POST /gateway/admin/providers/:id/key` | Save a key without verifying |
| `POST /gateway/admin/providers/:id/import` | Import a saved login: `commandcode`, `xiaomi-mimo`, `codex`, `kiro` |
| `POST /gateway/admin/providers/:id/models/sync` | Re-fetch the provider's model list now |
| `GET /gateway/admin/providers/:id/models` | `models`, `custom_models`, `seed_models`, `synced_models`, `models_synced_at` |
| `POST /gateway/admin/providers/:id/models` | `{"model":"…"}`: add a custom model |
| `PUT /gateway/admin/providers/:id/models` | `{"models":[…]}`: replace custom models |
| `DELETE /gateway/admin/providers/:id/models` | `{"model":"…"}`, or `DELETE …/models/:model` |

Extra keys are managed with the account endpoints below (toggle, reorder, delete), using the
provider id.

## Accounts (subscriptions)

| | |
|---|---|
| `GET /gateway/admin/accounts` | `accounts` (id, provider, label, email, enabled, plan, `cooling_until`, `last_error`), `providers` (id, alias, name, flow), `models`, `aliases`, `provider_enabled` |
| `GET /gateway/admin/accounts/usage?days=7&tz_offset_min=420&force=true` | Per account: `plan`, `quotas` (name, used, total, `remaining_percentage`, `reset_at` in unix seconds, unlimited), `quota_message`, `today_tokens`, `days` (tokens per local day, oldest first), `cooling_until`. `tz_offset_min` is minutes east of UTC; `force` skips the 2-minute quota cache |
| `POST /gateway/admin/accounts/:provider/start` | Begin a login → `session_id`, `kind` (`redirect`/`device`), `auth_url` or `verification_uri` + `user_code`, `interval` |
| `POST /gateway/admin/accounts/:provider/poll` | `{"session_id":"…"}` → `{"status":"pending"|"done"|"expired"}` (device logins) |
| `POST /gateway/admin/accounts/:provider/complete` | `{"session_id":"…","callback":"<pasted callback URL or code>"}` (browser logins, manual fallback) |
| `POST /gateway/admin/accounts/:provider/test` | `{"account_id":"…"}` → `{"valid":true,"latency_ms":…}` |
| `PATCH /gateway/admin/accounts/:provider/:id/toggle` | `{"enabled":false}`: pause / resume |
| `POST /gateway/admin/accounts/:provider/reorder` | `{"account_ids":["…","…"]}` |
| `DELETE /gateway/admin/accounts/:provider/:id` | Remove one account |

## Routing

| | |
|---|---|
| `GET /gateway/admin/settings` | `settings` + `auto_order` |
| `POST /gateway/admin/settings` | Any of `routing_strategy`, `fallback_order`, `provider_priority`, `auto_models_per_provider`, `cooldown_secs`, `gemini_oauth_client_id`, `gemini_oauth_client_secret`, `clear_gemini_secret`. Unknown values are **400** |
| `GET /gateway/admin/combos` | `{"combos":{"coding":["cx/gpt-5.5","cc/auto"]}}` |
| `POST /gateway/admin/combos` | `{"name":"coding","targets":[…]}` |
| `PUT /gateway/admin/combos/:name` | `{"targets":[…]}` |
| `DELETE /gateway/admin/combos/:name` | |
| `GET /gateway/admin/models` | Every model of every provider with `enabled` and `custom` flags |

## Usage and analytics

| | |
|---|---|
| `GET /gateway/admin/stats` | `requests`, `input_tokens`, `output_tokens`, `errors`, `active_keys`, `connected_providers` |
| `GET /gateway/admin/analytics?days=7&tz_offset_min=420` | `days` (date, requests, input, cached, output, cost), `models` (range), `top_models` (today), `recent` (last 20: ts, model, provider, input, output, ok, cost). Omit `days` for all history |
| `GET /gateway/admin/usage?limit=100` | Raw request log |
| `DELETE /gateway/admin/usage` | Clear the request log |

## Skills, Agents, Sessions & Live Activity

If `EAY_GATEWAY_ADMIN_TOKEN` (or `--admin-token`) is set, all `/gateway/admin/*` endpoints require `Authorization: Bearer <token>` or `x-admin-token: <token>`.
`EAY_GATEWAY_ADMIN_READ_TOKEN` (or `--admin-read-token`) adds a second token that can only make `GET` requests (stats, activity, lists), for dashboards and monitors; writes with it get `403`.
Without a token, `/gateway/admin/*` and the `x-eay-mcp` header only work when the `Host` header is `localhost`, `127.0.0.1` or `[::1]` (blocks DNS-rebinding from web pages); other hosts get `403`.

| | |
|---|---|
| `GET /gateway/admin/skills` | List saved Context/Skill presets |
| `POST /gateway/admin/skills` | `{"name":"rust","description":"…","prompt":"…"}` |
| `PUT /gateway/admin/skills/:name` | Create or update skill by name |
| `PATCH /gateway/admin/skills/:name/toggle` | Enable / disable a skill |
| `DELETE /gateway/admin/skills/:name` | Delete a skill |
| `GET /gateway/admin/agents` | List saved Agent Personality profiles |
| `POST /gateway/admin/agents` | `{"name":"senior-rust","model":"auto/coding","skills":["rust"],"personality":"…","temperature":0.2,"mcp":["filesystem"],"tool_policy":{"allow":["github/*"],"max_subagents":3}}` (usable via `model: "agent/senior-rust"`). `mcp` is the servers this agent uses on a loopback request |
| `PUT /gateway/admin/agents/:name` | Create or update agent by name |
| `DELETE /gateway/admin/agents/:name` | Delete an agent profile |
| `GET /gateway/admin/teams` | Rosters you assembled: `id` (`tm_…`), `name`, `lead`, `members`. Names may repeat |
| `POST /gateway/admin/teams` | `{"name":"app","lead":"lead","members":["lead","frontend","reviewer"]}`. Every member must already be a saved agent. The lead defaults to the first member. Send `id` to update that team, including a rename; omit `id` to create a new one |
| `DELETE /gateway/admin/teams/:id` | Delete a roster. Sessions that used it are left in place; their next request reports the team as missing |
| `GET /gateway/admin/sessions` | Sessions, newest first: `id` (storage id), `session`, `owner` (`local` or `key:<id>`), message count, `team` (roster name, if one was bound), `updated_at`. `?limit=` (default 100) |
| `GET /gateway/admin/sessions/:id` | One session's `messages` and the `subagents` that ran in it. Use the `id` from the list; a bare session name finds the local (no-key) one |
| `DELETE /gateway/admin/sessions/:id` | Clear saved conversation history for a session id |
| `GET /gateway/admin/events` | SSE stream of real-time request & subagent activity phases (`queued`, `preparing`, `routing`, `waiting_upstream`, `thinking`, `calling_tool`, `awaiting_approval`, `approved`, `denied`, `generating`, `failover`, `completed`, `failed`, `cancelled`), including structured `meta` (`action`, `file`, `lines`, `added`, `removed`, `command`, `duration_ms`) |
| `GET /gateway/admin/activity` | JSON snapshot of recent `events` and `subagents` |
| `GET /gateway/admin/subagents` | List active and recent background subagent runs |
| `POST /gateway/admin/subagents` | Spawn a background subagent `{"role":"Reviewer","model":"auto","prompt":"…","skills":[…],"mcp":["*"],"parent_id":"sub_…","api_key":"gw-sk-…"}`. `mcp` gives it MCP tools, `parent_id` links it into an agent tree, `api_key` (or header `x-eay-key`) makes it run under that key's budget and limits. At most 8 run at once; the rest wait in `queued` |
| `GET /gateway/admin/subagents/tree` | Runs nested by `parent_id` (`children` arrays). A request that delegates is the root, with its workers underneath |
| `GET /gateway/admin/subagents/:id` | Get subagent status and output. `?wait=60` long-polls up to that many seconds (max 300) until it finishes, so an orchestrator can join on a child |
| `POST /gateway/admin/subagents/:id/cancel` | Stop a queued or running subagent and every run it started (phase becomes `cancelled`) |
| `GET /gateway/admin/approvals` | Tool calls waiting for a person: `id`, `run_id`, `agent`, `tool`, `arguments`, `expires_at` |
| `POST /gateway/admin/approvals/:id` | `{"approve": true}` or `false`. `404` if already answered or expired |
| `DELETE /gateway/admin/subagents/:id` | Cancel it if still running and remove it from history |
| `GET /gateway/admin/mcp` | List registered external MCP servers (`stdio` / `http`) |
| `POST /gateway/admin/mcp` | Save an MCP server (`{"name":"notion","transport":"http","url":"https://mcp.notion.com/mcp","headers":{"Authorization":"Bearer …"}}` or `stdio` with `command`, `args`, `env`) |
| `PATCH /gateway/admin/mcp/:name/toggle` | Enable / disable an MCP server |
| `DELETE /gateway/admin/mcp/:name` | Delete an MCP server |
| `POST /gateway/admin/mcp/:name/tools` | Connect to the MCP server and list its available tools |

Tool names shown to the model are `mcp__<server>__<tool>`; names over 64 characters or with characters outside `[A-Za-z0-9_-]` are shortened to a stable `…_<hash>` form, which the gateway maps back to the real tool.

Remote (`http`) servers keep their `mcp-session-id` between calls and start a new session if the server answers `404`. Image results are passed to the model as images; audio and binary resources become a short note. When a local server fails, the error includes the last lines it wrote to stderr.

### Tool policies

A `tool_policy` on an agent or a virtual key decides which MCP tools the **gateway** runs for it
(`x-eay-mcp`, subagents with `mcp`). Tools that coding clients run themselves (Claude Code, Codex,
Cursor) are not affected; their permission modes stay in the client.

```json
{ "mode": "read_only", "allow": ["github/*", "fs/read_file"], "deny": ["delete_issue"] }
```

| Field | Effect |
|---|---|
| `mode` | `all` (default), `read_only` (only tools the server marks `readOnlyHint`), or `none` |
| `allow` | If set, only matching tools are offered: `server/tool`, `server/*`, or a bare `tool` name |
| `deny` | Matching tools are never offered |
| `approval` | `auto` (default) runs tools at once; `manual` waits for a person to approve every call; `manual_for_writes` waits only for tools the server does not mark read-only |
| `max_subagents` | Lets the model start subagents with the `eay_delegate` tool, up to this many per request. Unset or 0: off |

#### Approval

A tool call that needs approval stops and emits an `awaiting_approval` event with the approval
`id`. Answer it with `POST /gateway/admin/approvals/:id` (eay-console shows Approve/Deny buttons).
With no answer in 5 minutes it counts as denied, and the model is told not to retry. The request
stays open while it waits, so approval fits subagents and background jobs best; an interactive
client may time out first.

#### Subagents started by the model

With `max_subagents` set, the model gets an `eay_delegate` tool (`role`, `task`, optional `model` or
`agent`). Calling it starts a subagent, waits for it (up to 10 minutes) and returns its answer as the
tool result.

- **Rosters:** without `x-eay-team`, the tool lists your saved agents (name and description). With
  a roster, it lists only that roster's other members, and a call that names anyone else is rejected.
  With `agent: "tester"` the subagent runs as `agent/tester`, with that agent's model, skills,
  personality and tool policy. A saved agent is offered `delegate` only when its own policy sets
  `max_subagents` (a key can still cap it). It does not inherit the lead's budget. Its own subagents
  are temporary workers, not extra members of the roster.
- **Parallel:** every `eay_delegate` call in one model reply runs at the same time (still at most 8
  subagents active on the gateway), and the results come back in the order the model asked, each
  labelled with the worker's role and agent.
- **The tree:** once a request delegates, that request is the root of
  `GET /gateway/admin/subagents/tree` and its workers are `children`. Cancelling a run stops every
  run under it. If the client disconnects, the workers it was waiting on are stopped too.
- **Team thread:** the lead and every worker it started share one thread through the `eay_say` tool
  (post a note, or omit `message` to read). A worker sees the thread so far when it starts, including
  what its siblings were asked to do. A different request cannot read it. The thread is dropped when
  the lead run finishes.

Subagents:

- run under the same key, so its budget and limits apply;
- get a policy at least as strict as their parent's (strictest mode and approval, intersected
  `allow`, all `deny`), and may lower the subagent budget but never raise it;
- stop at three levels deep;
- appear in `GET /gateway/admin/subagents` with `parent_id` set to the run that started them.

When both the agent and the key have a policy, a tool must pass both. Disallowed tools are left
out of the request, and a call to one anyway is refused. `read_only` relies on the MCP server's own
`readOnlyHint`; a server that does not set it exposes nothing in that mode.

Local (`stdio`) MCP servers are started once and kept running between calls, so stateful servers (browsers, database sessions) keep their state. An idle process exits after 10 minutes; editing, disabling or deleting a server stops it. Calls to one server run one at a time; `tools/call` may take up to 120 s.
| `POST /gateway/admin/mcp/:name/call` | Run one tool directly: `{"tool":"read_file","arguments":{…}}` → `{"ok":true,"output":"…","duration_ms":12}` |
| `POST /gateway/admin/mcp/import` | Add every server from a Claude Desktop / Claude Code / Cursor (`mcpServers`) or VS Code (`servers`) config; returns `imported` and per-server `errors` |
