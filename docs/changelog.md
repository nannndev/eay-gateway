# Changelog

All notable changes to **eay-gateway** are listed below.

## v0.2.0 — 2026-11-18

### Highlights
- **Multi-Protocol Endpoints:** Added Anthropic-native `POST /v1/messages`, OpenAI `POST /v1/responses`, `/v1/embeddings`, `/v1/images/generations`, `/v1/audio/speech`, `/v1/audio/transcriptions`, and `/v1/audio/translations`.
- **Multimodal & Smart Routing:** Image, PDF/document, video, and audio translation for Anthropic, Gemini, and Codex; automatic plain-text/code attachment decoding for all providers; smart multimodal target filtering in `auto`.
- **Skills, Agents & Session Memory:** Reusable Context/Skill presets (`+skill`, `x-eay-skills`, `SKILL.md` upload), Agent Personalities (`agent/<name>`), and SQLite session memory (`x-eay-session-id`).
- **Guardrails & Budgets:** Virtual key `max_tokens_budget` and `max_cost_usd` enforcement, secret redaction (`x-eay-redact`), context trimming (`x-eay-max-context-chars`), and optional `--admin-token` protection.
- **Subagents & Rich Live Activity:** Background subagent runner (`/gateway/admin/subagents`) and SSE event stream (`/gateway/admin/events`) with structured `meta` (`action`, `file`, `lines`, `added`, `removed`, `command`, `duration_ms`).
- **External MCP Connectors:** Connect `stdio` and Streamable HTTP MCP servers (`/gateway/admin/mcp*`) with custom `headers`/`env` and automatic tool execution via `x-eay-mcp`.
- **Embedded Web Control Console:** Built-in Deep Dark Monochrome console at `/` and `/ui` covering OAuth logins, live quotas, routing/combos, skills, agents, MCP presets, virtual keys, and playground.
- **Modular Core:** Split `engine`, `translate`, and `store` into focused submodules.

## v0.1.0 — 2026-11-18
- Initial release: `/v1/chat/completions`, `/v1/models`, 14 OAuth subscription logins, 52 API-key providers, priority/round-robin/fill-first routing, AES-256-GCM encrypted store, and virtual keys.
