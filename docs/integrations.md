# Integrations

eay-gateway is a local HTTP server. Anything that can call the OpenAI or Anthropic API can use it:
change the base URL, keep your code. The gateway picks a connected account, fails over when one is
rate-limited, and tracks usage.

| | Base URL | Protocol |
|---|---|---|
| OpenAI SDKs and most tools | `http://127.0.0.1:20129/v1` | `/v1/chat/completions`, `/v1/responses`, `/v1/embeddings`, images, audio |
| Anthropic SDKs, Claude Code | `http://127.0.0.1:20129` | `/v1/messages` |

**API key:** any value works on your own machine. To give an app its own budget and limits, create
a [virtual key](api.md#virtual-keys) (`gw-sk-…`) and use that.

**Model:** `auto` picks the best connected provider. Use `<alias>/auto`, a specific
[model id](routing.md#model-ids) like `cc/claude-sonnet-5-5`, a combo `auto/<name>`, or an agent
`agent/<name>`. `GET /v1/models` lists what is available right now.

## Coding tools

### Claude Code

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:20129
export ANTHROPIC_API_KEY=gw-sk-…   # or any value locally
claude
```

Claude Code sends Claude model names. If a Claude subscription or Anthropic key is connected, they
go there; otherwise they are answered by `auto`, so Claude Code keeps working with other providers.

Claude Code's permission modes (plan, accept edits, bypass) run in Claude Code itself and work
unchanged. "Think" / "think hard" / "ultrathink" are forwarded as the matching reasoning effort to
whichever provider answers.

### Codex CLI

`~/.codex/config.toml`:

```toml
model = "auto"
model_provider = "eay"

[model_providers.eay]
name = "eay-gateway"
base_url = "http://127.0.0.1:20129/v1"
wire_api = "responses"
env_key = "EAY_GATEWAY_KEY"   # export EAY_GATEWAY_KEY=gw-sk-… (any value locally)
```

### Cursor

Settings → Models → OpenAI API key: turn on **Override OpenAI Base URL**, set it to
`http://127.0.0.1:20129/v1`, enter any key, and add `auto` (or another model id) as a custom
model.

### Cline, Roo Code, Continue, Zed, Open WebUI

Choose the **OpenAI Compatible** provider and set:

- Base URL: `http://127.0.0.1:20129/v1`
- API key: `gw-sk-…` (or any value locally)
- Model: `auto`

## Code

### Python

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:20129/v1", api_key="gw-sk-…")
reply = client.chat.completions.create(model="auto", messages=[{"role": "user", "content": "Hello"}])
print(reply.choices[0].message.content)
```

```python
from anthropic import Anthropic

client = Anthropic(base_url="http://127.0.0.1:20129", api_key="gw-sk-…")
msg = client.messages.create(model="auto", max_tokens=1024, messages=[{"role": "user", "content": "Hello"}])
print(msg.content[0].text)
```

### TypeScript / Node

```ts
import OpenAI from "openai";

const client = new OpenAI({ baseURL: "http://127.0.0.1:20129/v1", apiKey: "gw-sk-…" });
const stream = await client.chat.completions.create({
  model: "auto",
  messages: [{ role: "user", content: "Hello" }],
  stream: true,
});
for await (const chunk of stream) process.stdout.write(chunk.choices[0]?.delta?.content ?? "");
```

Vercel AI SDK:

```ts
import { createOpenAI } from "@ai-sdk/openai";
import { generateText } from "ai";

const eay = createOpenAI({ baseURL: "http://127.0.0.1:20129/v1", apiKey: "gw-sk-…" });
const { text } = await generateText({ model: eay.chat("auto"), prompt: "Hello" });
```

### LangChain

```python
from langchain_openai import ChatOpenAI

llm = ChatOpenAI(base_url="http://127.0.0.1:20129/v1", api_key="gw-sk-…", model="auto")
```

### Go

```go
client := openai.NewClient(
    option.WithBaseURL("http://127.0.0.1:20129/v1/"),
    option.WithAPIKey("gw-sk-…"),
)
```

### curl

```sh
curl http://127.0.0.1:20129/v1/chat/completions \
  -H 'content-type: application/json' -H 'authorization: Bearer gw-sk-…' \
  -d '{"model":"auto","messages":[{"role":"user","content":"Hello"}]}'
```

## Gateway features over plain HTTP

Add these headers to any request; SDKs let you set default headers on the client.

| Header | Effect |
|---|---|
| `x-eay-session-id: <id>` | The gateway remembers the conversation; send only new messages |
| `x-eay-skills: rust,reviewer` | Adds saved [skills](api.md) to the system prompt |
| `x-eay-mcp: *` (or server names) | Gives the model your MCP tools and runs them for it |
| `x-eay-redact: true` | Strips API keys and tokens from messages before they leave |
| `x-eay-max-context-chars: 200000` | Drops the oldest messages beyond this size |
| `x-eay-run-id: <id>` | Follow this request live on `GET /gateway/admin/events` |

```python
client = OpenAI(
    base_url="http://127.0.0.1:20129/v1",
    api_key="gw-sk-…",
    default_headers={"x-eay-session-id": "support-chat-42", "x-eay-mcp": "filesystem"},
)
```

## Desktop apps

Ship the gateway binary with your app and talk to it over HTTP from native code. Several apps can
share one gateway; follow [Sharing one gateway between apps](configuration.md#sharing-one-gateway-between-apps):

1. `GET http://127.0.0.1:20129/health`. If it answers with `"service":"eay-gateway"`, use it.
2. Otherwise start the bundled binary with `--idle-exit 30` and don't stop it when your app quits.

Call the admin API (`/gateway/admin/*`) from your app's native side (Rust, Swift, C#, Node main
process), not from a webview: requests without a browser `Origin` are not subject to the gateway's
browser rules. OpenNotchi works this way.

## Web apps

A page in the browser can call the gateway directly if its origin is allowed:

- `http://localhost:<port>` is always allowed (local development).
- Anything else needs `--console-origin https://your.app` when you start the gateway.
- Other sites get `403`, so a page you happen to visit cannot use your gateway.

[eay-console](https://github.com/nannndev/eay-console) is a complete example: one HTML file that
manages providers, keys, MCP servers and agents through the admin API.

## Agents and orchestration

Permission modes in coding tools (Claude Code's plan mode, Codex's approval modes, Cursor's
Ask/Agent) are handled by those tools and need nothing from the gateway. When the **gateway** runs
tools for you (`x-eay-mcp`, subagents), limit them with a [tool policy](api.md#tool-policies) on
the agent or key, for example a read-only reviewer:

```sh
curl -X POST http://127.0.0.1:20129/gateway/admin/agents -H 'content-type: application/json' -d '{
  "name": "reviewer", "model": "auto", "personality": "Review code, never change it.",
  "tool_policy": { "mode": "read_only" }
}'
```

The admin API runs background agents for you:

```sh
# start a child agent with MCP tools, under a key's budget
curl -X POST http://127.0.0.1:20129/gateway/admin/subagents -H 'content-type: application/json' -d '{
  "role": "Reviewer", "model": "auto", "prompt": "Review src/lib.rs",
  "mcp": ["filesystem"], "api_key": "gw-sk-…", "parent_id": "my-run-1"
}'

# wait up to 2 minutes for it to finish
curl 'http://127.0.0.1:20129/gateway/admin/subagents/sub_…?wait=120'
```

See [HTTP API](api.md) for cancel, limits and the live event stream.

## Rust, in-process

Use `eay-gateway-core` directly, without HTTP. See
[Embedding the core in Rust](architecture.md#embedding-the-core-in-rust).
