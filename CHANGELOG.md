# Changelog

All notable changes to `eay-gateway` are documented in this file.

## [Unreleased]

### Security
- Without an admin token, `/gateway/admin/*` and the `x-eay-mcp` header only accept loopback `Host` headers. This blocks DNS rebinding from a web page, which could otherwise register a local (stdio) MCP server and run a command.
- Admin token is compared in constant time. New read-only admin token (`--admin-read-token` / `EAY_GATEWAY_ADMIN_READ_TOKEN`) that can only make `GET` requests.
- `x-eay-session-id` memory is private to each virtual key (and to the no-key local caller).

### Added
- Per-key `max_concurrent` (requests in flight; streamed responses hold their slot until they end).
- Subagents: `POST /gateway/admin/subagents/:id/cancel`, `GET …/:id?wait=N` to join on a run, `parent_id`, `mcp`, `api_key` (runs under that key's limits), at most 8 running at once, history kept across restarts.
- `x-eay-run-id` header to follow your own request on `/gateway/admin/events`.
- MCP: `POST /gateway/admin/mcp/import` (Claude Desktop / Claude Code / Cursor / VS Code configs), `POST /gateway/admin/mcp/:name/call` to run one tool directly.
- Web console rebuilt around every API, with a streaming playground (three protocols, attachments, live route trace, embeddings/image/speech/transcription).
- End-to-end tests against a mock upstream and mock MCP servers.

### Changed
- `/v1/responses` streams real Responses events (text, function calls, usage) and forwards function tools, `tool_choice` and `function_call` history, so Codex CLI tool use works through the gateway.
- Local MCP servers stay running between calls (stateful servers keep their state); remote ones reuse their `mcp-session-id`. Tool lists are cached.
- MCP tool names over 64 characters are shortened to a stable form; image results reach the model as images; errors include the server's stderr.
- `rate_limit_per_min` counts client requests, not failover attempts.
- Request body limit raised from 2 MB to 64 MB. Upstreams silent for 300 s are cut off; connect timeout 10 s.
- `~` in MCP commands and arguments expands to the home folder.

### Fixed
- Session memory is now saved for streamed replies (it only worked for non-streaming requests).

### Upgrade notes
- Sessions saved before this version are not loaded any more (they were not tied to a key). Delete them with `DELETE /gateway/admin/sessions/:id`.
- If you reach the admin API through a hostname other than `localhost`/`127.0.0.1` without `--admin-token`, it now returns `403`. Set an admin token.

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
