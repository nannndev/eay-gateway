# Changelog

All notable changes to **eay-gateway** are listed below.

## Unreleased

### Highlights
- **Safer by default:** admin API and MCP tools are loopback-only without an admin token (blocks DNS rebinding); new read-only admin token; session memory is private per key.
- **Codex CLI works end to end:** `/v1/responses` streams real Responses events and carries function tools and tool-call history.
- **Multi-agent:** cancel, join (`?wait=`), parent trees, MCP access and per-key limits for subagents; at most 8 at once; history survives restarts.
- **Multi-access:** per-key `max_concurrent`; rate limits count requests, not failover attempts.
- **MCP:** servers stay running between calls, config import from other apps, run a tool directly, long tool names shortened, image results passed as images, stderr in errors.
- **Limits:** 64 MB request bodies, 300 s upstream silence cut-off.
- **Console:** rebuilt around every API with a streaming playground and live route trace.

Upgrade notes: older sessions are not loaded (they were not tied to a key); reaching admin through a non-loopback hostname without `--admin-token` now returns `403`.

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
