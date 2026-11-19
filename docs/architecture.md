# Architecture

## Crates

```
crates/
├── eay-gateway-core/      library: everything that isn't HTTP
│   ├── providers.rs       provider catalog: ids, aliases, base URLs, families, built-in models
│   ├── store/             SQLite store (models, accounts, providers, access/skills/agents/sessions, usage)
│   ├── cipher.rs          AES-256-GCM for secrets at rest
│   ├── engine/            orchestrator (mod: tool loop, delegate, approval gate), routing, stream/usage guard,
│   │                      guardrails, activity (events, subagent runs, approvals), state
│   ├── translate/         multimodal media, thinking/reasoning, OpenAI ⇄ Anthropic, Responses, Gemini, Command Code; SSE translators
│   ├── mcp.rs             MCP client (stdio processes kept alive, Streamable HTTP sessions), tool policies
│   ├── kiro.rs            Kiro request format and AWS event-stream decoder (CRC-checked)
│   ├── oauth/             logins (browser, device code, imports), token refresh, per-provider requests
│   ├── quota.rs           live usage windows per account, cached, keep-last-good
│   ├── models.rs          key verification and model list sync
│   └── pricing.rs         estimated cost per request
└── eay-gateway/           HTTP server and the `eay-gateway` binary
    ├── routes/            axum handlers: /v1 and /gateway/<resource> (accounts, agents, keys, …), admin-token, CORS and per-key concurrency middleware
    ├── error.rs           GatewayError → HTTP status + JSON body
    └── main.rs            flags, data folder + migration, lock, idle exit, hourly model refresh
```

`eay-gateway-core` has no HTTP server dependency. `GatewayEngine::chat_completions` returns
`ChatReply::Stream(ChatStream)` / `ChatReply::Json(Value)` or a `ChatError { status, body }`, and
the server crate maps those to axum responses.

## Request flow

```
POST /v1/chat/completions
  │  browser Origin check (CORS allowlist) → per-key max_concurrent slot
  │  virtual key check (gw-sk-…: enabled, allowed providers, rate limit, budgets)
  │  agent/<name> → model, skills, personality, tool policy; session history prepended
  │  thinking / reasoning effort normalised for the upstream's format
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

## Gateway-run tools and subagents

When a request asks for MCP tools (`x-eay-mcp`) or its agent/key allows delegation, the engine
runs the tool loop itself instead of returning tool calls to the client:

```
policies = agent.tool_policy + key.tool_policy + inherited (from a parent run)
offered  = MCP tools every policy permits  (+ eay_delegate if max_subagents > 0 and depth < 3)
loop (max 8 rounds):
  model turn → tool calls?
    none          → reply to the client (and save the session)
    eay_delegate  → spawn children in parallel (same key, stricter policy, depth + 1) → wait → result
    MCP tool      → still permitted?  → needs approval? → wait for POST /approvals/:id (5 min, else deny)
                                      → call_tool → result (images become image parts)
```

A subagent is a background run with its own id, `parent_id` and `parent_session`. It takes one of
8 slots, can be cancelled, and is saved to `subagent_runs` when it ends. Cancelling a run stops
every run under it. A request that delegates is stored as the root of that tree. A saved agent
delegates only when its own policy sets `max_subagents`. The lead and its workers share one
in-memory thread (`eay_say`); other runs cannot read it. An agent's `mcp` list is added only when
the request is allowed to run local MCP (loopback, same rule as `x-eay-mcp`).

A saved **team** is a roster of agent names (`x-eay-team`), separate from that thread and from the
subagents an agent starts. Its id (`tm_…`) stays put when the name changes. A session stores the
chat and that id. Closing a session drops the chat only. Two sessions can use the same roster and
do not see each other's messages.

## Storage

One SQLite file, `gateway.db`:

| Table | Holds |
|---|---|
| `providers` | API-key providers: encrypted main key, base URL, enabled, custom models, synced models + time, last check |
| `accounts` | Subscription accounts and extra API keys: provider, label, email, encrypted credential JSON, order, enabled, last error |
| `usage` | One row per upstream attempt (a failed failover is its own row): provider, model, tokens (input, output, cached), estimated cost, duration, status, virtual key, `agent` (saved name or worker role), `team` (roster id). `GET /gateway/usage` omits cached tokens and cost; analytics returns them and groups `agents` and `teams` |
| `keys` | Virtual keys: SHA-256 of the secret, limits, budgets (`max_tokens_budget`, `max_cost_usd`), `max_concurrent`, `default_skills`, `tool_policy`, totals |
| `combos` | Combo name → JSON list of targets |
| `skills` | Context/Skill presets: name, description, prompt, enabled |
| `agents` | Agent profiles: description, model, skills, personality, temperature, max_tokens, `tool_policy`, `mcp` |
| `teams` | Rosters: id (`tm_…`), name (a label, not unique), lead, members |
| `sessions` | Conversation memory: `key:<key-id>:<session>` for virtual keys, `local:<session>` without a key. `team` stores a roster id |
| `subagent_runs` | Finished subagent runs (newest 500), restored into memory at startup |
| `mcp_servers` | External MCP connectors (`stdio` command/args/env or `http` url/headers) |
| `settings` | Routing settings, Gemini OAuth client |

Secrets (`providers.api_key`, `accounts.secret`) are AES-256-GCM encrypted with `gateway.key`.
Schema changes are additive (`ALTER TABLE … ADD COLUMN`, errors ignored), so older databases
upgrade in place.

In memory only: cooldowns, round-robin cursors, the fill-first "last good account", the quota
cache, pending OAuth sessions, per-key rate-limit windows and in-flight counts, running subagents,
pending approvals (lost on restart, which counts as denied), live MCP processes and HTTP MCP
sessions, and the MCP tool-list cache.

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
