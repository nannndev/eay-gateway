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
    title: Multi-Protocol & Multimodal
    details: Speaks /v1/chat/completions, /v1/messages (Anthropic native), /v1/responses, /v1/embeddings, images, and audio. Translates images, PDFs, video and thinking settings for every provider.
  - icon: ⛨
    title: Budgets, Guardrails & Encryption
    details: AES-256-GCM encrypted credentials, virtual keys with token/USD budgets, optional secret redaction, context trimming, and --admin-token protection.
  - icon: ◫
    title: Agents That Delegate
    details: Agent profiles with skills and tool policies, session memory, and subagents the model can start itself. Tools that change things can wait for your approval in eay-console.
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
    <h3>Headless Daemon</h3>
    <p>Single Rust binary with AES-256 store, /v1/chat/completions, /v1/messages, /v1/responses, /v1/embeddings, multimodal routing, and an admin API. The web console is a separate app (eay-console).</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-ready">Current Core</span>
    <h3>Agents, Delegation &amp; Approval</h3>
    <p>Agent profiles (agent/&lt;name&gt;) with skills and tool policies, session memory, subagents the model can start itself, manual approval for tools, and real-time SSE activity.</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-upcoming">Future Direction</span>
    <h3>Desktop Tray &amp; Vibecoding Studio</h3>
    <p>Native macOS/Windows menubar wrapper and workspace-aware AI vibecoding IDE powered directly by eay-gateway-core.</p>
  </div>

  <div class="roadmap-card">
    <span class="tag tag-ready">Current Core</span>
    <h3>Local Models</h3>
    <p>Ollama, and any OpenAI-compatible local server (vLLM, llama.cpp, LM Studio) through the custom provider, sit in the same routing and combos as cloud accounts.</p>
  </div>
</div>

</div>
