# Changelog

All notable changes to `eay-gateway` are documented in this file.

## [0.2.0] - 2026-11-18

### Added
- **Multi-Protocol Endpoints:**
  - `POST /v1/messages` (`/gateway/v1/messages`) — Anthropic-native Messages endpoint (streaming SSE & JSON) with automatic resolution for bare `claude-*` model names.
  - `POST /v1/responses` (`/gateway/v1/responses`) — OpenAI Responses API endpoint.
  - `POST /v1/embeddings`, `/v1/images/generations`, `/v1/audio/speech`, `/v1/audio/transcriptions`, `/v1/audio/translations`.
- **Multimodal & Smart Routing:**
  - Request translation for images, PDFs/documents, video, and audio across Anthropic, Gemini, Codex, and Command Code.
  - Automatic base64 decoding for plain-text and source-code file attachments (`normalize_text_documents`) so all text-only models can read them.
  - Automatic multimodal target filtering (`filter_multimodal_targets`) when using `auto`.
- **Skills, Agent Profiles & Session Memory:**
  - Reusable Context/Skill presets (`/gateway/admin/skills`), activatable via `model+skill`, `x-eay-skills`, or virtual-key `default_skills`, plus `SKILL.md` upload in Web Console.
  - Agent Personality Profiles (`/gateway/admin/agents`, usable via `model: "agent/<name>"`) combining model/combo, skills, personality prompt, `temperature`, and `max_tokens`.
  - SQLite-backed conversation memory via `x-eay-session-id` (`DELETE /gateway/admin/sessions/:id`).
- **Guardrails, Budgets & Security:**
  - Virtual key token and USD budgets (`max_tokens_budget`, `max_cost_usd`).
  - Optional secret redaction (`x-eay-redact: true`) and context window trimming (`x-eay-max-context-chars`).
  - Optional admin authentication via `--admin-token <TOKEN>` or `EAY_GATEWAY_ADMIN_TOKEN`.
- **Subagents & Rich Live Activity Stream:**
  - Background subagent runner (`/gateway/admin/subagents`) and real-time SSE activity feed (`/gateway/admin/events`, `/gateway/admin/activity`).
  - Rich activity phases (`queued`, `preparing`, `routing`, `waiting_upstream`, `thinking`, `calling_tool`, `generating`, `failover`, `completed`, `failed`) with structured `meta` (`action`, `file`, `lines`, `added`, `removed`, `command`, `duration_ms`).
- **External MCP Connectors:**
  - Connect local `stdio` and remote Streamable HTTP MCP servers (`/gateway/admin/mcp*`) with custom `env`/`headers`, `mcp-session-id` handshake, and automatic tool execution loop via `x-eay-mcp`.
- **Embedded Web Control Console & VitePress Docs:**
  - Built-in Deep Dark Monochrome console at `/` and `/ui` (Overview, OAuth logins & live quotas, API-key providers, Routing & Combos, Skills, Agents, Subagents & Activity, MCP presets, Virtual Keys, Playground).
  - VitePress documentation & landing page in `docs/` with GitHub Pages workflow.

### Refactored
- Split monolithic `engine.rs`, `translate.rs`, and `store.rs` into focused submodules (`engine/`, `translate/`, `store/`).

## [0.1.0] - 2026-11-18
- Initial release with `/v1/chat/completions`, `/v1/models`, 14 OAuth subscription logins, 52 API-key providers, routing strategies, AES-256-GCM credential store, and virtual keys.
