# eay-gateway

One local, OpenAI-compatible endpoint for all your AI subscriptions and API keys.

Point Claude Code, Codex, Cursor, OpenNotchi or any OpenAI client at `http://127.0.0.1:20129/v1`.
The gateway sends each request to one of your connected accounts and moves on to the next one
when an account is rate-limited, out of quota or down.

- **14 subscription logins** (Claude, ChatGPT/Codex, GitHub Copilot, Gemini CLI, Kimi, Cline, Kiro, …)
  and **52 API-key providers** (OpenAI, Anthropic, DeepSeek, OpenRouter, Groq, NVIDIA, …)
- **Routing:** `auto` across everything, one provider (`cx/auto`), or your own mix (`auto/coding`),
  with priority, round-robin or fill-first, and cooldown after rate limits
- **Several accounts and keys per provider**, rotated and paused independently
- **Live quotas** per account (remaining %, reset time, plan), usage history and estimated cost
- **Model lists synced** from each provider
- Translates between OpenAI, Anthropic, Codex Responses, Gemini, Command Code and Kiro formats,
  streaming or not

## Quick start

```sh
git clone https://github.com/nannndev/eay-gateway && cd eay-gateway
cargo run --release --bin eay-gateway       # http://127.0.0.1:20129
```

Connect a provider, then ask something:

```sh
curl -X POST http://127.0.0.1:20129/gateway/admin/providers/deepseek/connect \
  -H 'content-type: application/json' -d '{"api_key":"sk-..."}'

curl http://127.0.0.1:20129/v1/chat/completions -H 'content-type: application/json' \
  -d '{"model":"auto","messages":[{"role":"user","content":"Hello"}]}'
```

Prebuilt binaries for macOS, Windows and Linux are on the
[releases page](https://github.com/nannndev/eay-gateway/releases).

## Documentation

| | |
|---|---|
| [Getting started](docs/getting-started.md) | Install, run, connect accounts and keys, use it from your tools |
| [Configuration](docs/configuration.md) | Flags, environment variables, data folder, sharing one gateway between apps |
| [Routing](docs/routing.md) | Model ids, auto modes, combos, strategies, fallback and cooldown |
| [Providers](docs/providers.md) | Every provider, its alias, how to log in, quotas and model sync |
| [HTTP API](docs/api.md) | `/v1` OpenAI-compatible API and the `/gateway/admin` management API |
| [Architecture](docs/architecture.md) | Crates and modules, request flow, storage, embedding the core in Rust |
| [Development](docs/development.md) | Building, testing, releasing, adding a provider |

To preview the documentation & landing page locally:
```sh
npm install
npm run docs:dev    # http://localhost:5173
```

## Security

The management API has no authentication, so keep the gateway bound to `127.0.0.1` (the default).
API keys and OAuth tokens are encrypted on disk with AES-256-GCM, using a key file in the data folder.
Clients of `/v1` may present gateway-issued virtual keys (`gw-sk-…`) with per-key provider limits
and rate limits.

## Experimental providers

Antigravity, Grok CLI, Kiro and MiniMax Code identify as their official clients to reach private
endpoints. They can stop working whenever those clients change, and using them may break the
provider's terms of service or get an account flagged.

## License

MIT
