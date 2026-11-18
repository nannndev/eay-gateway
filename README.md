# eay-gateway

One local, OpenAI-compatible endpoint for all your AI subscriptions and API keys.
Point Claude Code, Codex, Cursor, OpenNotchi or any OpenAI client at `http://127.0.0.1:20129/v1`
and the gateway routes each request to a connected account, falling back when one is rate-limited.

## Quick start

```sh
cargo run --release --bin eay-gateway          # listens on 127.0.0.1:20129
curl http://127.0.0.1:20129/v1/models
```

| Flag | Env | Default |
|---|---|---|
| `--host` | `EAY_GATEWAY_HOST` | `127.0.0.1` |
| `--port` | `EAY_GATEWAY_PORT` | `20129` |
| `--data-dir` | `EAY_GATEWAY_DATA_DIR` | `~/.eay-gateway` (`%APPDATA%\eay-gateway` on Windows) |

The older `NOTCHI_GATEWAY_*` variables still work. On first run the gateway moves an existing
`~/.notchi-gateway` folder to the new location, so connected accounts and keys carry over.

## Architecture

```
crates/
├── eay-gateway-core/   library: everything that isn't HTTP
│   ├── providers.rs    catalog of API-key providers and subscription (OAuth) providers
│   ├── store.rs        SQLite: accounts, keys (AES-GCM encrypted), usage log, settings, combos
│   ├── engine.rs       routing: auto / <alias>/auto / auto/<combo>, strategies, fallback, cooldown
│   ├── translate.rs    OpenAI <-> Anthropic, Codex Responses, Gemini, Command Code, SSE streams
│   ├── kiro.rs         Kiro request format and AWS event-stream decoder
│   ├── oauth/          browser and device-code logins, token refresh, per-provider requests
│   ├── quota.rs        live usage windows (remaining %, reset time, plan) per account
│   ├── models.rs       key checks and model lists synced from each provider
│   └── pricing.rs      estimated API-equivalent cost per request
└── eay-gateway/        HTTP server (axum) + the `eay-gateway` binary
    ├── routes/         /v1/* (OpenAI-compatible) and /gateway/admin/* (management API)
    └── main.rs         CLI flags, data folder, hourly model refresh, graceful shutdown
```

- **Rust apps** can depend on `eay-gateway-core` and call `GatewayEngine::chat_completions`
  in-process; it returns a `ChatReply` (SSE stream or JSON) or a `ChatError` with an HTTP status.
- **Everything else** (Swift, Node, Python, …) runs the `eay-gateway` binary and talks HTTP.
  This is how OpenNotchi uses it on macOS and Windows.

### HTTP API

| Area | Endpoints |
|---|---|
| OpenAI-compatible | `POST /v1/chat/completions`, `GET /v1/models` |
| Health | `GET /health` → `{service, status, version}` |
| Providers (API keys) | `GET /gateway/admin/providers`, `POST …/:id/connect`, `…/:id/keys`, `…/:id/models/sync`, `…/:id/import` |
| Accounts (OAuth) | `GET /gateway/admin/accounts`, `POST …/:provider/start`, `…/poll`, `…/complete`, `…/reorder`, `…/test`, `GET …/usage` |
| Routing | `GET/POST /gateway/admin/settings`, `GET/POST /gateway/admin/combos` |
| Insight | `GET /gateway/admin/stats`, `GET /gateway/admin/analytics` |
| Virtual keys | `GET/POST /gateway/admin/keys` (clients present `gw-sk-…` keys) |

The admin API has no authentication, so keep the gateway bound to `127.0.0.1`.

### Model ids

- `auto`: every connected provider, in your priority order
- `<alias>/auto`: one provider, e.g. `cx/auto`
- `auto/<combo>`: your own ordered mix, e.g. `auto/coding`
- `<alias>/<model>`: one model, e.g. `cc/claude-sonnet-5-5`

## Development

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Releases: push a `vX.Y.Z` tag. CI builds `eay-gateway` for macOS (arm64, x86_64, universal),
Windows x64 and Linux x64, and attaches the binaries plus `SHA256SUMS.txt` to the GitHub release.

## Experimental providers

Antigravity, Grok CLI, Kiro and MiniMax Code identify as their official clients to reach
private endpoints. They can stop working when those clients change, and using them may break
the provider's terms or get an account flagged. Use them with that in mind.
