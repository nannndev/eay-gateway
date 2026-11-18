---
layout: home

hero:
  name: "eay-gateway"
  text: "One Unified AI Gateway for Subscriptions & API Keys"
  tagline: "Point Claude Code, Cursor, Codex, or any OpenAI client to a single local endpoint. Auto-failover across 14 subscriptions and 52 providers."
  image:
    src: /logo.svg
    alt: eay-gateway logo
  actions:
    - theme: brand
      text: Quick Start →
      link: /getting-started
    - theme: alt
      text: Routing & Auto Modes
      link: /routing
    - theme: alt
      text: GitHub Repo
      link: https://github.com/nannndev/eay-gateway

features:
  - icon: ⇄
    title: Intelligent Auto-Routing
    details: Automatically routes requests with priority, round-robin, or fill-first. Failed or rate-limited accounts enter cooldown while healthy accounts take over seamlessly.
  - icon: ⌘
    title: 14 Subscriptions + 52 Providers
    details: Mix subscription logins (Claude, ChatGPT/Codex, Copilot, Kimi, Cline...) with API keys (DeepSeek, OpenAI, Groq, NVIDIA...) under custom combos like auto/coding.
  - icon: ◈
    title: Real-time Quotas & Cost Tracking
    details: Inspect remaining percentages, quota reset windows, active subscription tiers, and estimated cost per account through the admin API.
  - icon: ⚡
    title: Universal Format Translation
    details: Speaks OpenAI, Anthropic, Gemini, Codex, Command Code, and Kiro formats interchangeably. Streaming and non-streaming responses are fully translated.
  - icon: ⛨
    title: Local & AES-256 Encrypted
    details: Zero cloud dependency. Binds safely to 127.0.0.1 by default. Stored credentials are encrypted on disk with AES-256-GCM using a local key.
  - icon: ◫
    title: Extensible & Future-Ready
    details: Written in high-performance Rust. Built with an architecture ready to power future web dashboards, desktop GUI wrappers, and local model bridges.
---

<div class="custom-section">

## Quick Connect in Seconds

Point any client or coding assistant to `http://127.0.0.1:20129/v1` and use model `auto`.

::: code-group

```sh [cURL]
# 1. Connect a provider
curl -X POST http://127.0.0.1:20129/gateway/admin/providers/deepseek/connect \
  -H 'content-type: application/json' \
  -d '{"api_key":"sk-..."}'

# 2. Chat with auto-routing
curl http://127.0.0.1:20129/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "auto",
    "messages": [{"role": "user", "content": "Explain rust ownership"}]
  }'
```

```json [Cursor / VS Code (.cursorrules)]
{
  "openai_base_url": "http://127.0.0.1:20129/v1",
  "openai_api_key": "gw-sk-local",
  "model": "auto/coding"
}
```

```sh [Claude Code CLI]
export ANTHROPIC_BASE_URL="http://127.0.0.1:20129"
export ANTHROPIC_API_KEY="gw-sk-local"
claude
```

```python [Python (OpenAI SDK)]
from openai import OpenAI

client = OpenAI(
    base_url="http://127.0.0.1:20129/v1",
    api_key="gw-sk-local"
)

response = client.chat.completions.create(
    model="auto",
    messages=[{"role": "user", "content": "Hello gateway!"}]
)
print(response.choices[0].message.content)
```

:::

</div>

<div class="custom-section">

## Comprehensive Provider Support

Mix and rotate dozens of accounts across both subscription logins and pay-per-token API providers.

<div class="provider-chips">
  <span class="provider-chip highlight">◆ Claude Pro / Team</span>
  <span class="provider-chip highlight">◆ ChatGPT Plus / Team</span>
  <span class="provider-chip highlight">◆ GitHub Copilot</span>
  <span class="provider-chip highlight">◆ Gemini CLI</span>
  <span class="provider-chip highlight">◆ Kimi / Moonshot</span>
  <span class="provider-chip highlight">◆ Cline & Kiro</span>
  <span class="provider-chip">OpenAI</span>
  <span class="provider-chip">Anthropic</span>
  <span class="provider-chip">DeepSeek</span>
  <span class="provider-chip">OpenRouter</span>
  <span class="provider-chip">Groq</span>
  <span class="provider-chip">Mistral</span>
  <span class="provider-chip">NVIDIA NIM</span>
  <span class="provider-chip">Together AI</span>
  <span class="provider-chip">Cohere</span>
  <span class="provider-chip">Azure OpenAI</span>
  <span class="provider-chip">Cloudflare Workers AI</span>
  <span class="provider-chip">+ 40 more</span>
</div>

</div>

<div class="custom-section">

## How Request Routing Works

Requests come in from your tools through the standard OpenAI `/v1` format. The gateway identifies your model intent, validates quota state, and routes to an available backend.

```
Client (Cursor, Claude, IDE)
            │
            ▼
┌──────────────────────────────────────────────┐
│       eay-gateway (127.0.0.1:20129)          │
│                                              │
│  1. Format Translation (OpenAI/Anthropic/...)│
│  2. Strategy (Priority / Round-Robin / Fill) │
│  3. Cooldown & Live Quota Check              │
└──────────────────────┬───────────────────────┘
                       │
         ┌─────────────┼─────────────┐
         ▼             ▼             ▼
  [Claude Login]  [DeepSeek API] [OpenAI Key 2]
    (Cooldown)     (Successful)     (Standby)
```

</div>

<div class="custom-section">

## Vision & Roadmap

`eay-gateway` starts as a lightweight, zero-overhead background daemon. As the project evolves, the architecture is designed to support rich management experiences and deeper system integrations.

<div class="roadmap-grid">
  <div class="roadmap-card">
    <span class="tag tag-ready">Current Core</span>
    <h3>High-Performance Daemon</h3>
    <p>Zero-dependency Rust binary, AES-256 credential store, cross-format translation, and resilient failover engine.</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-progress">In Exploration</span>
    <h3>Web Admin Interface</h3>
    <p>A local browser-based dashboard to manage connected accounts, view quota gauges, monitor requests, and test model combos.</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-upcoming">Future Direction</span>
    <h3>Desktop Tray & Menubar App</h3>
    <p>Quick toggle from your macOS/Windows menu bar: switch routing presets, see remaining token limits, and view gateway status.</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-upcoming">Future Direction</span>
    <h3>Local LLM Bridging</h3>
    <p>Seamlessly blend local runtimes (Ollama, vLLM, llama.cpp) as zero-cost fallback tiers when cloud quotas are exhausted.</p>
  </div>
</div>

</div>
