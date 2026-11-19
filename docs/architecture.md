# Architecture

## Crates

```
crates/
├── eay-gateway-core/      library: everything that isn't HTTP
│   ├── providers.rs       provider catalog: ids, aliases, base URLs, families, built-in models
│   ├── store/             SQLite store (models, accounts, providers, access/skills/agents/sessions, usage)
│   ├── cipher.rs          AES-256-GCM for secrets at rest
│   ├── engine/            orchestrator (mod), routing, stream/usage guard, guardrails, activity & subagents, state
│   ├── translate/         multimodal media, OpenAI ⇄ Anthropic, Codex Responses, Gemini, Command Code; SSE translators
│   ├── mcp.rs             external MCP JSON-RPC client (stdio & Streamable HTTP)
│   ├── kiro.rs            Kiro request format and AWS event-stream decoder (CRC-checked)
│   ├── oauth/             logins (browser, device code, imports), token refresh, per-provider requests
│   ├── quota.rs           live usage windows per account, cached, keep-last-good
│   ├── models.rs          key verification and model list sync
│   └── pricing.rs         estimated cost per request
└── eay-gateway/           HTTP server and the `eay-gateway` binary
    ├── routes/            axum handlers: /v1, /gateway/admin, admin-token, CORS and per-key concurrency middleware
    ├── error.rs           GatewayError → HTTP status + JSON body
    └── main.rs            flags, data folder + migration, lock, idle exit, hourly model refresh
```

`eay-gateway-core` has no HTTP server dependency. `GatewayEngine::chat_completions` returns
`ChatReply::Stream(ChatStream)` / `ChatReply::Json(Value)` or a `ChatError { status, body }`, and
the server crate maps those to axum responses.

## Request flow

```
POST /v1/chat/completions
  │  virtual key check (gw-sk-…: enabled, allowed providers, rate limit)
  ▼
engine.resolve_targets(model)         auto | <alias>/auto | auto/<combo> | <alias>/<model>
  ▼
engine.plan_attempts(targets)         × accounts / keys, by strategy and fallback order,
  │                                     cooling ones moved last
  ▼
for each attempt:
  ├─ subscription: fresh_credential() → oauth::subscription_request()  (refresh + retry on 401)
  └─ API key:      main or extra key → provider format (OpenAI, Anthropic, Responses, Command Code)
  ▼
  non-2xx → log usage, maybe cooldown, record account error, try next
  2xx     → SseToOpenAi / KiroStream translate the upstream stream to OpenAI chunks
  ▼
stream: true  → SSE straight to the client; usage logged when the stream ends (or the client leaves)
stream: false → frames aggregated into one chat.completion
```

## Storage

One SQLite file, `gateway.db`:

| Table | Holds |
|---|---|
| `providers` | API-key providers: encrypted main key, base URL, enabled, custom models, synced models + time, last check |
| `accounts` | Subscription accounts and extra API keys: provider, label, email, encrypted credential JSON, order, enabled, last error |
| `usage` | One row per attempt (failover attempts included): provider, model, tokens (input, output, cached), cost, duration, status, virtual key |
| `keys` | Virtual keys: SHA-256 of the secret, limits, budgets (`max_tokens_budget`, `max_cost_usd`), `max_concurrent`, `default_skills`, totals |
| `combos` | Combo name → JSON list of targets |
| `skills` | Context/Skill presets: name, description, prompt, enabled |
| `agents` | Agent Personality profiles: model, skills, personality, temperature, max_tokens |
| `sessions` | Conversation memory: `key:<key-id>:<session>` for virtual keys, `local:<session>` without a key |
| `subagent_runs` | Finished subagent runs (newest 500), restored into memory at startup |
| `mcp_servers` | External MCP connectors (`stdio` command/args/env or `http` url/headers) |
| `settings` | Routing settings, Gemini OAuth client |

Secrets (`providers.api_key`, `accounts.secret`) are AES-256-GCM encrypted with `gateway.key`.
Schema changes are additive (`ALTER TABLE … ADD COLUMN`, errors ignored), so older databases
upgrade in place.

In memory only: cooldowns, round-robin cursors, the fill-first "last good account", the quota
cache, pending OAuth sessions, per-key rate-limit windows and in-flight counts, running subagents,
live MCP processes and HTTP MCP sessions, and the MCP tool-list cache.

## Embedding the core in Rust

```toml
[dependencies]
eay-gateway-core = { git = "https://github.com/nannndev/eay-gateway" }
```

```rust
use eay_gateway_core::{ChatReply, GatewayEngine, GatewayStore};

let engine = GatewayEngine::new(GatewayStore::new("/path/to/data")?);
match engine.chat_completions(None, serde_json::json!({
    "model": "auto",
    "messages": [{"role": "user", "content": "Hi"}],
})).await {
    Ok(ChatReply::Json(reply)) => println!("{reply}"),
    Ok(ChatReply::Stream(_)) => unreachable!("stream was not requested"),
    Err(e) => eprintln!("{}: {}", e.status, e.body),
}
```

The core bundles SQLite (`rusqlite` with `bundled`). An app that links a different `rusqlite`
version will hit a native-library conflict; run the binary instead in that case.
