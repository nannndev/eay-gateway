# HTTP API

Base URL: `http://127.0.0.1:20129`. All bodies are JSON. Errors look like
`{"detail":"…","error":{"message":"…"}}`, or OpenAI-style `{"error":{"message":"…","type":"gateway_error"}}`
on `/v1`.

## OpenAI-compatible API

### `POST /v1/chat/completions`

OpenAI Chat Completions request: `model`, `messages` (text, images, tool calls, tool results),
`tools`, `temperature`, `max_tokens`, `stream`. See [Routing](routing.md) for `model`.

- `stream: true`: server-sent `chat.completion.chunk` events ending with `data: [DONE]`
- `stream: false`: a single `chat.completion` with `usage`

Responses come back in OpenAI format whatever the upstream speaks. When every attempt fails the
status is **502** with the last upstream error in the message.

Also served at `/gateway/v1/chat/completions`.

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

| | |
|---|---|
| `GET /gateway/admin/keys` | List keys (prefix only, plus usage totals) |
| `POST /gateway/admin/keys` | `{"name":"ci","allowed_providers":["deepseek"],"rate_limit_per_min":30}` → returns the secret `key` once |
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
