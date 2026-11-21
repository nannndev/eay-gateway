# eay-gateway-core

`eay-gateway-core` is the library behind the `eay-gateway` binary. It contains everything that
isn't HTTP: the provider catalog, encrypted storage, OAuth logins, routing with failover,
wire-format translators, MCP tools, quotas and pricing. The server crate (`crates/eay-gateway`)
is a thin axum layer over it. Use this page when you change the core or embed it in a Rust app.
For the big picture, see [Architecture](/architecture).

```toml
[dependencies]
eay-gateway-core = { git = "https://github.com/nannndev/eay-gateway" }
```

Crate-root re-exports:

```rust
pub use cipher::Cipher;
pub use engine::{ChatError, ChatReply, ChatStream, GatewayEngine};
pub use providers::{OAuthProviderMeta, Provider, Target, OAUTH_PROVIDERS, PROVIDERS};
pub use store::{AccountRecord, GatewaySettings, GatewayStore, ProviderRecord, VirtualKeyRecord};
```

## Module map

| Module | Purpose | Main items |
|---|---|---|
| [`engine`](#engine) | Routes OpenAI-style requests across providers/accounts: failover, cooldowns, agents, skills, sessions, MCP tool loop, subagents | `GatewayEngine`, `ChatOptions`, `ChatReply`, `ChatError`, `ActivityHub` |
| [`store`](#store) | SQLite persistence (`<dir>/gateway.db`) for every record type | `GatewayStore`, `*Record`, `UsageEntry` |
| [`cipher`](#cipher) | AES-256-GCM for secrets at rest | `Cipher` |
| [`providers`](#providers) | Static catalog of API-key and subscription providers; model-string parsing | `PROVIDERS`, `OAUTH_PROVIDERS`, `Target`, `parse_model_string` |
| [`oauth`](#oauth) | Logins (PKCE redirect, device code, CLI/IDE import), token refresh, subscription request builders | `OAuthManager`, `fresh_credential`, `subscription_request` |
| [`translate`](#translate) | OpenAI ⇄ Anthropic / Responses (Codex) / Gemini / Antigravity / Command Code, SSE translation, media | `openai_to_*`, `SseToOpenAi`, `aggregate_sse` |
| [`mcp`](#mcp) | MCP client (stdio + Streamable HTTP), tool naming, tool policies | `ToolPolicy`, `McpToolIndex`, `call_tool_rich` |
| [`workspace`](#workspace) | Project-folder file tools exposed as the `eay_project` MCP server | `tools`, `call`, `prepare_root` |
| [`kiro`](#kiro) | Kiro request format and AWS event-stream decoder | `openai_to_kiro`, `EventStreamDecoder`, `KiroStream` |
| [`quota`](#quota) | Live usage windows per subscription account | `QuotaCache`, `AccountQuota` |
| [`models`](#models) | Key verification, model-list sync | `verify_upstream_provider`, `sync_provider_models` |
| [`pricing`](#pricing) | Estimated USD cost per request | `cost` |

## Embedding

```rust
use eay_gateway_core::{ChatReply, GatewayEngine, GatewayStore};

let store = GatewayStore::new("/path/to/data")?;   // creates dir, gateway.db, gateway.key, runs migrations
let engine = GatewayEngine::new(store);
engine.restore_subagent_history();                 // once at startup

match engine.chat_completions(None, serde_json::json!({
    "model": "auto",
    "messages": [{"role": "user", "content": "Hi"}],
})).await {
    Ok(ChatReply::Json(reply)) => println!("{reply}"),
    Ok(ChatReply::Stream(_)) => unreachable!("stream was not requested"),
    Err(e) => eprintln!("{}: {}", e.status, e.body),
}
```

The first argument is the caller's `VirtualKeyRecord` (`None` = no key: no budgets, sessions
stored as `local:<id>`). Look a key up with `store.find_virtual_key(secret)`.

The binary also runs `models::sync_stale_models` hourly. Call it yourself if you want model lists
to stay fresh. The core bundles SQLite (`rusqlite` + `bundled`), so an app that links a different
`rusqlite` will get a native-library conflict. Run the binary instead in that case.

## engine

```rust
#[derive(Clone)]
pub struct GatewayEngine {
    pub store: Arc<GatewayStore>,
    pub http_client: reqwest::Client,   // UA eay-gateway/<ver>, connect 10 s, read idle 300 s
    pub oauth: oauth::OAuthManager,
    pub quotas: quota::QuotaCache,
    pub activity: ActivityHub,
    /* private: round-robin cursors, fill-first sticky account, MCP tool cache, in-flight, rate window */
}
```

### Entry points

| Method | Use |
|---|---|
| `new(store) -> Self` | Build the engine. Cheap to `clone` (all `Arc`). |
| `chat_completions(key, body)` | Default options. |
| `chat_completions_with_skills(key, body, header_skills)` | Adds skills from the key, `x-eay-skills`, or a `+skill` model suffix. |
| `chat_completions_with_options(key, body, ChatOptions)` | Full entry point: `agent/<name>` profiles, skills, session memory, context trimming, redaction, MCP tool loop, delegate/say. |
| `passthrough_openai_post(key, path, model, PassthroughPayload)` | Forwards `embeddings`, `images/generations`, `audio/speech`, `audio/transcriptions` to OpenAI-family API-key providers with failover. Returns `PassthroughReply { status, content_type, body }`. |
| `admit_rate(key)` | Counts one request against `rate_limit_per_min` (sliding 60 s, in memory). Call once per client request. 429 when exceeded. |
| `try_acquire_inflight(key) -> Option<InflightGuard>` | Takes one of the key's `max_concurrent` slots. The guard frees it on drop. |
| `spawn_subagent(key, role, model, prompt, system, parent_id, opts) -> SubagentRun` | Background run under the key's budgets. Waits for one of 8 slots. Saved when it ends. |
| `cancel_subagent(id) -> bool` | Cancels a run and everything under it. |
| `restore_subagent_history()` | Loads the last 200 saved runs into memory. |
| `resolve_targets(model) -> Vec<Target>` | Expands `auto`, `<alias>/auto`, `auto/<combo>`, `<alias>/<model>` (see [Routing](/routing)). |
| `auto_provider_order(settings)`, `auto_model_ids()` | Provider order and picker list for auto modes. |

```rust
pub enum ChatReply { Stream(ChatStream), Json(Value) }
pub type ChatStream = BoxStream<'static, Result<Bytes, std::io::Error>>; // chat.completion.chunk SSE, ends with [DONE]
pub struct ChatError { pub status: u16, pub body: Value }  // gateway refusals: {"error":{"message":..,"type":"gateway_error"}}
```

Usage is logged by a guard inside `ChatStream`, so a client that disconnects mid-stream is still
recorded. `session_storage_id(key, sid)` returns the session key (`key:<id>:<sid>` or `local:<sid>`).

### ChatOptions

| Field | Meaning |
|---|---|
| `skills`, `session_id` | Skill presets to inject; conversation memory id |
| `max_context_chars` | Trim older non-system messages above this size (`0` = off) |
| `redact_secrets` | Mask `sk-…`, `sk-ant-…`, `ghp_…`, `AIza…`, `AKIA…`, `xoxb-…`, `gw-sk-…` in user messages |
| `mcp_servers`, `mcp_allowed` | Gateway-run MCP servers; whether local (stdio) MCP may start (loopback callers only) |
| `team`, `team_members`, `project` | `x-eay-team` roster, `x-eay-project` (a key bound to a project overrides it) |
| `run_id`, `agent_label`, `inherited_policy`, `depth` | Set for subagents; a child never gets more than its parent |

### Limits and rules

| Constant | Value | Meaning |
|---|---|---|
| `MAX_MCP_ROUNDS` | 8 | Tool-call rounds per request (the body is forced to `stream:false` inside the loop) |
| `MAX_DELEGATE_DEPTH` | 3 | Deepest `eay_delegate` chain |
| `MAX_CONCURRENT_SUBAGENTS` | 8 | Running subagents at once |
| `APPROVAL_TIMEOUT_SECS` | 300 | An unanswered approval counts as denied |
| `DELEGATE_WAIT_SECS` | 600 | How long a delegate call waits for its children |
| MCP `tools/list` cache | 300 s | Per server |

- **Budgets:** a key is refused (429) once input+output tokens ≥ `max_tokens_budget` or its cost ≥ `max_cost_usd`.
- **Cooldowns:** bucket = account id or `apikey:<provider>`. They start on a 429 or on "rate limit / quota /
  usage limit" text. The length is `Retry-After`, then `resets_in_seconds`, then `settings.cooldown_secs`,
  capped at 6 h. Cooling targets are moved **last**, not skipped.
- **Token refresh:** on an upstream 401 the engine forces one refresh and retries once.
- **Tool policies:** agent, key and inherited policies all apply and the strictest wins (strictest mode and
  approval, intersected allow lists, all denies, smallest subagent budget).
- **Sessions** store what was said, not reasoning. A failed or empty reply does not save a dangling user turn.
- **Prompt order:** project instructions, then agent personality, then skills.

### ActivityHub

The in-memory event bus and subagent registry behind `/gateway/events`, `/gateway/runs` and approvals.

| Method | Use |
|---|---|
| `subscribe()` | `broadcast::Receiver<ActivityEvent>` (capacity 256) |
| `recent_events()` | Last ~100 events |
| `emit`, `emit_phase`, `emit_with_meta` | Publish an event. Phases: `queued, preparing, routing, waiting_upstream, thinking, generating, calling_tool, awaiting_approval, failover, completed, failed` |
| `list_approvals()`, `resolve_approval(id, approve)` | Pending tool approvals. `resolve` is false if unknown, already answered or expired |
| `list_runs()`, `run_tree()`, `get_run(id)`, `delete_run(id)` | Runs; `run_tree` nests children under `children`, newest roots first |
| `cancel_tree(id)`, `finish_run(id, output, error)`, `wait_run(id, timeout)` | Lifecycle. A run ends only through `finish_run` or cancel; per-turn events don't end it |
| `team_post(root, from, msg)` | The shared `eay_say` thread (40 notes × 500 chars, dropped when the root finishes) |

## store

```rust
let store = GatewayStore::new(dir)?;  // Clone-able; holds Arc<Mutex<Connection>> + Cipher
```

`store` also exports `KEY_PREFIX = "gw-sk-"`, `now() -> f64` (unix seconds) and `hash_key(secret)` (SHA-256 hex).

| Area | Methods |
|---|---|
| Accounts | `list_accounts`, `save_account`, `update_or_save_account`, `record_account_error`, `remove_account`, `reorder_accounts`, `set_account_enabled` |
| Cooldowns (memory) | `cool_down(key, secs)`, `clear_cooldown`, `cooling_until` |
| Providers | `list_providers`, `connect_provider`, `disconnect_provider`, `save_provider_key`, `get_provider_key`, `get_provider_record`, `set_provider_enabled`, `set_provider_verification`, `set/get_synced_models`, `display_models`, custom-model add/remove/set |
| Combos | `list_combos`, `save_combo`, `delete_combo` |
| Virtual keys | `create_virtual_key` (returns the plaintext secret **once**), `find_virtual_key(secret)`, `list_virtual_keys`, `toggle_virtual_key`, `delete_virtual_key`, `set_virtual_key_{limits,tool_policy,concurrency,skills}`, `key_total_cost`, `key_requests_since`, `touch_virtual_key` |
| Skills / Agents / Teams | `list_*`, `get_*`, `save_*`, `delete_*`, `toggle_skill`. `save_agent` validates the policy (≤ 20 MCP names, `*` = all enabled). `save_team(None, …)` makes a `tm_…` id; members must be saved agents |
| Projects | `list/get/save/delete_project`, `set_project_link("keys"|"teams", id, project)`. Name 1–100 chars, instructions ≤ 20 000 bytes, `metadata` a JSON object, `root` checked with `workspace::prepare_root` first. Delete unlinks but keeps keys/teams/sessions/usage |
| Sessions | `get/save_session_messages`, `list_sessions(limit)`, `set_session_title`, `session_project`, `session_team`, `delete_session` |
| Subagent runs | `save_subagent_run` (keeps newest 500), `list_subagent_runs`, `delete_subagent_run` |
| MCP servers | `list/get/save/toggle/delete_mcp_server` (name `[A-Za-z0-9_-]`) |
| Settings | `get_gateway_settings` (alias `get_settings`), `get_setting`, `set_setting` |
| Usage | `record_usage(&UsageEntry)` (cost from `pricing::cost`), `list_usage`, `clear_usage`, `get_stats`, `analytics(days, utc_offset)`, `daily_tokens_by_provider`, `project_usage(project, since)` |

`GatewaySettings`: `routing_strategy` (`priority` \| `round_robin` \| `fill_first`), `fallback_order`
(`model_first` \| `account_first`), `provider_priority`, `auto_models_per_provider` (2),
`cooldown_secs` (60), Gemini OAuth client.

Read queries log errors and return empty results instead of panicking. Migrations are additive
(`ALTER TABLE … ADD COLUMN`), so older databases upgrade in place. Table layout:
[Architecture → Storage](/architecture#storage).

## cipher

```rust
let c = Cipher::new(dir)?;           // loads or creates <dir>/gateway.key (32 raw bytes, 0600 on unix)
let blob = c.encrypt("secret")?;     // base64(nonce12 || ciphertext), fresh nonce per call
let plain = c.decrypt(&blob)?;
```

If you lose `gateway.key`, every stored provider key and account credential is unreadable. Back
it up together with `gateway.db`.

## providers

- `PROVIDERS`: API-key providers (`Provider { id, alias, name, family, base_url, needs_key, seed_models }`).
- `OAUTH_PROVIDERS`: 14 subscription providers (`OAuthProviderMeta { id, alias, plan, flow, family, default_model, … }`).
- `find_provider_by_id/alias`, `find_oauth_by_id/alias` (aliases include legacy ones such as `km`, `qw`, `nim`, `glm`).
- `parse_model_string("<alias|id>/<model>") -> Target { provider, model, kind: "apikey"|"oauth" }`. OAuth aliases are checked first. `Target::public()` gives `"<alias>/<model>"`.

| id | alias | flow | family |
|---|---|---|---|
| claude | cc | redirect | anthropic |
| codex | cx | redirect | codex |
| copilot | gh | device | openai |
| gemini-cli | gc | redirect | gemini |
| kimi | km | device | openai |
| muse | mu | device | openai |
| nous | ns | device | openai |
| kilocode | kc | device | openai |
| cline | cl | redirect | openai |
| minimax-code / minimax-code-global | mmc / mmcg | device | anthropic |
| antigravity | ag | redirect | gemini |
| grok-cli | gcli | device | openai |
| kiro | kr | device | kiro |

## oauth

- `OAuthManager`: pending login sessions (expiring) and one callback listener per port. `poll_allowed`
  enforces the RFC 8628 polling interval.
- Flows return JSON for the UI on start and save the account on completion:
  `start/complete_{claude_pkce, codex_pkce, gemini_cli, antigravity, cline}_flow`, `start/poll_copilot_device_flow`,
  `start/poll_device_flow` (kimi, muse, nous, grok-cli, kiro, …), `complete_generic_token_flow`,
  `import_codex_from_cli` (`~/.codex/auth.json`), `import_kiro_from_ide` (`~/.aws/sso/cache`).
- Redirect ports: Claude and Google use `localhost:54545/callback`, Codex uses `localhost:1455/auth/callback`.
  A newer login replaces a pending one on the same port.
- `fresh_credential(client, store, acc, force)` refreshes when the token expires within 5 min (or `force`)
  and saves it if it changed.
- `subscription_request(client, provider, cred, model, body) -> Option<(RequestBuilder, format)>`, where
  format is `codex` \| `claude` \| `gemini` \| `openai` and tells the caller which SSE translator to use.

::: warning
Several subscription clients (Kiro, Grok CLI, Command Code, Antigravity) pin the user agents and versions
of the official apps. These can break, or get an account flagged, when the vendor changes its client checks.
:::

## translate

| Function / type | Converts |
|---|---|
| `openai_to_anthropic`, `openai_to_claude_oauth` | Chat request → Anthropic Messages (OAuth variant identifies as Claude Code, which Claude OAuth tokens require) |
| `anthropic_to_openai`, `openai_completion_to_anthropic`, `OpenAiToAnthropicSse` | `/v1/messages` in, Anthropic-shaped replies and SSE out |
| `openai_to_codex`, `CodexStreamTranslator` | Chat → Codex Responses upstream, and its stream back |
| `responses_to_openai`, `openai_completion_to_responses`, `OpenAiToResponsesSse` | `/v1/responses` in and out (`response.created … response.completed`) |
| `openai_to_gemini`, `antigravity_envelope` | Gemini CLI / Antigravity Cloud Code shapes |
| `openai_to_commandcode` | Command Code (keep `thread_id` stable per conversation) |
| `SseToOpenAi::new(kind, model)` | Upstream SSE (`openai`, `codex`, `claude`, `gemini`, `commandcode`, `kiro`) → OpenAI chunks. Tracks `usage` and `cached` |
| `aggregate_sse(sse, usage)` | Folds chunks into one `chat.completion` for `stream:false` |
| `detect_media_requirements`, `normalize_text_documents` | Media routing (keep only media-capable targets) and base64 text files → text parts |
| `reasoning_effort`, `budget_to_effort`, `effort_to_budget` | Effort ⇄ token budget: minimal 1024, low 4096, medium 10 000, high 32 000 |
| `strip_gateway_fields` | Removes gateway-only fields before raw OpenAI passthrough |

## mcp

- `encode_tool_name(server, tool)` gives `mcp__<server>__<tool>` (hashed short form when > 64 chars). Resolve shown names
  with `McpToolIndex::resolve`, not `parse_tool_name`.
- `ToolPolicy { mode: "read_only"|"none", allow, deny, approval: "auto"|"manual"|"manual_for_writes", max_subagents }`;
  `permits`, `needs_approval`, and the multi-policy helpers `all_permit`, `any_needs_approval`, `subagent_budget`.
- `list_tools_indexed`, `call_tool_rich` (text + images as `data:` URLs), `shutdown_server(name)` after editing a server.
- `parse_client_config` imports `mcpServers` (Claude/Cursor), `servers` (VS Code) or a bare map.
- Protocol `2024-11-05`. Stdio processes are pooled per `name@updated_at`, dropped after 600 s idle, and `kill_on_drop`.
- Policies apply only to tools the **gateway** runs. Client-side tools (Claude Code, Codex, Cursor) are never affected.

## workspace

File tools confined to a project's `root`, offered as MCP server `eay_project` so policies, approvals and
activity events work the same way: `list_dir`, `read_file`, `grep` (read-only), `write_file`, `edit_file`.

- `prepare_root(root, create)` canonicalises and checks (or creates) the folder. Call it before `save_project`.
- Refuses `..`, absolute paths outside the root, and symlinks that point out.
- Caps: reads 256 KB, grep 200 hits, listings 500 entries. Skips `.git, node_modules, target, dist, build, .next, .venv, __pycache__`.
- `edit_file` needs a unique `old_string`. Zero matches or several matches are both errors.

## kiro

`openai_to_kiro` builds Kiro's `conversationState` (strictly alternating turns; system text is folded into the first
user turn). `EventStreamDecoder` parses the binary AWS event stream (CRC-32 checked, frames ≤ 16 MiB).
`KiroStream` holds tool calls until the end, because Kiro streams their JSON in fragments.

## quota

`QuotaCache::get(client, store, acc, force) -> Option<AccountQuota { plan, quotas: Vec<Quota>, message, fetched_at }>`.
120 s cache. A failed read keeps the last good windows and is not cached. An expired token is refreshed once.
`quota::supports(provider)` covers claude, codex, copilot, gemini-cli, antigravity, kimi, kiro, cline, grok-cli.
`quota::parse` is pure and unit-tested.

## models

`verify_upstream_provider(client, id, key, base_url) -> (latency_ms, model_ids)`,
`sync_provider_models(engine, id)` (error if the list is empty), `sync_stale_models(engine)` (lists older than a day,
skips Ollama), `parse_model_ids` (OpenAI `data`, `models`, Ollama tags).

## pricing

`cost(model, input, output, cached) -> f64`: USD estimate from a hand-kept table (most specific id wins), where
`input` includes `cached`. Unknown models cost 0. Subscriptions don't bill per token, so this is "what it would cost
on the API", not an invoice.

## Tests

About 248 tests, all inline `#[cfg(test)]`, except `store/tests.rs` and `translate/tests.rs`. Store tests
use `tempfile` directories.

| Area | Tests | Covers |
|---|---|---|
| `translate` | 68 | every translator, SSE kinds, cached tokens, multimodal, thinking |
| `engine` | 46 | target resolution, subagents, MCP cache, session namespacing, guardrails, cooldowns |
| `store` | 44 | accounts, providers, keys, cooldowns, settings, analytics, projects, migrations |
| `oauth` | 24 | JWT claims, callback parsing, device polling, model-id mapping |
| `kiro`, `quota`, `mcp`, `models`, `cipher`, `pricing`, `workspace` | 23, 17, 9, 6, 4, 4, 3 | |

```sh
cargo test -p eay-gateway-core
```

## Changing the core

- Keep it free of HTTP-server types. Return `ChatError { status, body }` and let the server crate map it.
- Schema changes stay additive: add a column with `ALTER TABLE … ADD COLUMN` in the store migrations,
  never drop or rename.
- A new provider is a `PROVIDERS` / `OAUTH_PROVIDERS` entry, plus a translator in `translate/` if its wire
  format is new, plus a test in `translate/tests.rs`.
- Anything that changes routing or limits also goes in [Routing](/routing) and this page.
