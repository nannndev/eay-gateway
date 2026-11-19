# eay-gateway

One local, OpenAI-, Anthropic-, and Responses-compatible AI gateway for all your subscriptions and API keys — with multimodal routing, reusable Skills/Agents, guardrails, MCP tools, and live subagent activity. The web console is a separate app: [eay-console](https://github.com/nannndev/eay-console).

Point **Claude Code**, **Codex**, **Cursor**, **OpenNotchi**, or any OpenAI/Anthropic SDK at `http://127.0.0.1:20129`.
The gateway sends each request to one of your connected accounts and automatically fails over to the next one when an account is rate-limited, out of quota, or down.

## Key Features

- **14 subscription logins** (Claude, ChatGPT/Codex, GitHub Copilot, Gemini CLI, Kimi, Cline, Kiro, …)
  and **52 API-key providers** (OpenAI, Anthropic, DeepSeek, OpenRouter, Groq, NVIDIA, …)
- **Multi-protocol endpoints:**
  - `POST /v1/chat/completions` (OpenAI Chat Completions)
  - `POST /v1/messages` (Anthropic native — plug-and-play for Claude Code & Anthropic SDKs)
  - `POST /v1/responses` (OpenAI Responses API)
  - `POST /v1/embeddings`, `/v1/images/generations`, `/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/audio/translations`
- **Thinking everywhere:** Claude Code's "think hard", OpenAI `reasoning_effort` and Codex `reasoning.effort` reach Claude, Codex, Gemini and OpenAI-compatible providers; reasoning streams back to the client
- **Smart Routing & Failover:** `auto`, `<alias>/auto`, custom combos (`auto/coding`), priority / round-robin / fill-first, and automatic cooldowns after rate limits
- **Multimodal & Code-Ready:** Translates images, PDFs, video, and audio across Anthropic, Gemini, and Codex; automatically decodes plain-text/code file attachments for text-only models and skips text-only providers when `auto` receives binary media
- **Agents, Skills & Session Memory:**
  - Reusable **Skills** (`+skill`, `x-eay-skills`, or upload `SKILL.md` in eay-console)
  - **Agent Profiles** (`agent/<name>`) combining model/combo, skills, personality, tuning and a tool policy
  - **Session Memory** via `x-eay-session-id`, private per key, browsable through `GET /gateway/admin/sessions`
- **Multi-agent:** subagents you start (cancel, wait, parent trees, history) or that the model starts itself with the `eay_delegate` tool; children never get more permission than their parent and stop at three levels
- **Tool control for gateway-run tools:** per agent or key, `read_only` / allow / deny lists, and **manual approval** (`manual` or `manual_for_writes`) with Approve/Deny in eay-console. Coding apps keep their own permission modes
- **Guardrails & Budgets:** Virtual keys (`gw-sk-…`) with per-key `allowed_providers`, `rate_limit_per_min`, `max_concurrent`, `max_tokens_budget`, `max_cost_usd`, private session memory, plus optional secret redaction (`x-eay-redact: true`) and context trimming (`x-eay-max-context-chars`)
- **External MCP Connectors (`x-eay-mcp`):** Connect local `stdio` MCP servers (Filesystem, GitHub, Postgres) and Streamable HTTP remote MCP servers (Notion, Linear, Figma) with custom auth headers and automatic tool execution loops; local servers stay running between calls, and configs import straight from Claude Desktop, Claude Code, Cursor or VS Code
- **Web Console & Live Activity:** [eay-console](https://github.com/nannndev/eay-console) (a separate web app) logs in via OAuth, shows live quotas, configures routing & combos, manages skills/agents/MCP/keys, has a Playground, and watches **real-time subagent & request phases** (`preparing`, `routing`, `thinking`, `calling_tool` with file/line/diff metadata, `awaiting_approval`, `generating`, `failover`, `completed`) over SSE

## Quick start

```sh
brew install nannndev/tap/eay-gateway && brew services start eay-gateway
# or: download a binary from Releases, then `eay-gateway service install` to start it at login
```

Connect a provider, then send a request:

```sh
curl -X POST http://127.0.0.1:20129/gateway/admin/providers/deepseek/connect \
  -H 'content-type: application/json' -d '{"api_key":"sk-..."}'

curl http://127.0.0.1:20129/v1/chat/completions -H 'content-type: application/json' \
  -d '{"model":"auto","messages":[{"role":"user","content":"Hello"}]}'
```

Or point Claude Code CLI directly at the gateway:

```sh
export ANTHROPIC_BASE_URL="http://127.0.0.1:20129"
export ANTHROPIC_API_KEY="gw-sk-local"
claude
```

Prebuilt binaries for macOS, Windows and Linux are on the
[releases page](https://github.com/nannndev/eay-gateway/releases). For a home server, NAS or
Raspberry Pi there is a Docker image: see [Getting started](docs/getting-started.md#on-a-server-nas-or-raspberry-pi-docker).

## Documentation

| | |
|---|---|
| [Getting started](docs/getting-started.md) | Install, run, connect accounts and keys, use it from your tools |
| [Integrations](docs/integrations.md) | Claude Code, Codex, Cursor, Cline, SDKs (Python, TS, Go), desktop and web apps, agents, delegation and approval |
| [Configuration](docs/configuration.md) | Flags, environment variables, data folder, sharing one gateway between apps |
| [Routing](docs/routing.md) | Model ids, auto modes, combos, strategies, fallback and cooldown |
| [Providers](docs/providers.md) | Every provider, its alias, how to log in, quotas and model sync |
| [HTTP API](docs/api.md) | `/v1` inference APIs and `/gateway/admin` management: keys, agents, tool policies, subagents, approvals, sessions & live activity |
| [Architecture](docs/architecture.md) | Crates and modules, request flow, storage, embedding the core in Rust |
| [Development](docs/development.md) | Building, testing, releasing, adding a provider |

To preview the VitePress documentation & landing page locally:
```sh
npm install
npm run docs:dev    # http://localhost:5173
```

## Security

- By default the gateway binds to `127.0.0.1` without admin authentication, and admin routes accept only loopback `Host` headers. If you expose it beyond localhost, pass `--admin-token <TOKEN>` (or set `EAY_GATEWAY_ADMIN_TOKEN`) to protect `/gateway/admin/*`. Add `--admin-read-token <TOKEN>` for a token that can only read (dashboards, monitors).
- Browser pages can only call the gateway from `http://localhost:<port>` or origins you allow with `--console-origin` (for a hosted [eay-console](https://github.com/nannndev/eay-console)); other sites get `403`.
- API keys and OAuth tokens are encrypted on disk with AES-256-GCM, using a key file in the data folder.
- Clients of `/v1/*` may present gateway-issued virtual keys (`gw-sk-…`) with per-key provider limits, rate limits, token/USD budgets, and default skills.

## Experimental providers

Antigravity, Grok CLI, Kiro and MiniMax Code identify as their official clients to reach private
endpoints. They can stop working whenever those clients change, and using them may break the
provider's terms of service or get an account flagged.

## License

MIT
