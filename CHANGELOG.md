# Changelog

All notable changes to `eay-gateway` are documented in this file.

## [Unreleased]

### Added
- **Agent teams:** a model with a subagent budget can hand work to your saved agents
  (`eay_delegate` lists them and takes `agent`), and all delegations in one reply run in parallel.
  The request shows up as the root of the run tree. Cancelling a run stops the runs under it, and
  a saved agent delegates only when its own policy allows it. The lead and its workers share a
  team thread (`eay_say`). An agent can list the MCP servers it uses on a loopback request.
- **Rosters:** `POST /gateway/teams` saves a list of agents under a stable id (`tm_…`).
  `x-eay-team` limits who a request may delegate to. A session stores that id and keeps its own
  transcript, so renaming the team or sharing it across two sessions does not mix the chats.
- Cancelling a lead while its workers are running stops the request there. The model is not asked
  for a wrap-up turn. A provider call that is already in flight still finishes.
- `GET /gateway/subagents/tree`: runs nested by parent, for team views.
- **Usage by speaker:** each request log row stores the agent (or a worker's role) and the roster id.
  `GET /gateway/analytics` adds `agents` and `teams`, each with the upstream models they used.
  Older rows have neither and stay out of those lists.

### Changed
- Management routes dropped the `/admin` segment. `/gateway/admin/teams` is now
  `/gateway/teams`, and the same for accounts, providers, keys, agents, sessions, and the rest.
  `/v1`, `/gateway/v1`, and `/gateway/health` are unchanged. The admin token is unchanged.

## [0.3.1] - 2026-11-19

### Changed
- **Linux binaries are static** (musl + rustls): one file with no glibc or OpenSSL dependency, so
  it runs on any distribution, including Ubuntu 20.04/22.04, Debian 11/12 and Alpine. The 0.3.0
  Linux binary needed glibc 2.39 (Ubuntu 24.04+).
- New **Linux ARM64** binary (`eay-gateway-aarch64-unknown-linux-musl`) for Raspberry Pi and ARM VPS.
- HTTPS to providers uses rustls with the system's certificates, falling back to built-in roots
  when the system has none. The Docker image no longer needs OpenSSL.

The x64 Linux binary is still also published as `eay-gateway-x86_64-unknown-linux-gnu`, so existing
download links keep working.

## [0.3.0] - 2026-11-19

### Security
- Without an admin token, `/gateway/admin/*` and the `x-eay-mcp` header only accept loopback `Host` headers. This blocks DNS rebinding from a web page, which could otherwise register a local (stdio) MCP server and run a command.
- Admin token is compared in constant time. New read-only admin token (`--admin-read-token` / `EAY_GATEWAY_ADMIN_READ_TOKEN`) that can only make `GET` requests.
- `x-eay-session-id` memory is private to each virtual key (and to the no-key local caller).

### Added
- **Model-driven subagents:** with `tool_policy.max_subagents`, the model gets an `eay_delegate` tool to hand subtasks to subagents; they inherit a policy at least as strict as their parent's and stop at depth 3.
- **Manual approval** for gateway-run tools: `tool_policy.approval` (`auto`, `manual`, `manual_for_writes`), `GET /gateway/admin/approvals`, `POST /gateway/admin/approvals/:id`; unanswered calls are denied after 5 minutes.
- **Sessions API:** `GET /gateway/admin/sessions` and `GET /gateway/admin/sessions/:id` (messages and the session's subagents).
- **Thinking / reasoning effort works with every provider:** Claude Code's `thinking`, OpenAI `reasoning_effort` and Codex `reasoning.effort` are translated for Claude, Codex, Gemini and OpenAI-compatible upstreams; reasoning streams back to clients, and Claude's signed thinking is replayed with tool results.
- **Tool policies** for gateway-run MCP tools on agents and keys: `mode` (`all`, `read_only`, `none`), `allow` and `deny` lists; the stricter of agent and key wins.
- **Install and run:** Homebrew formula (`brew install nannndev/tap/eay-gateway`), a container image on `ghcr.io/nannndev/eay-gateway` (amd64/arm64) with `compose.yaml`, and `eay-gateway service install|uninstall|status` to start at login on macOS, Linux and Windows. `--version` flag.
- **Integrations guide** for Claude Code, Codex, Cursor, Cline, SDKs, desktop and web apps.
- Per-key `max_concurrent` (requests in flight; streamed responses hold their slot until they end).
- Subagents: `POST /gateway/admin/subagents/:id/cancel`, `GET …/:id?wait=N` to join on a run, `parent_id`, `mcp`, `api_key` (runs under that key's limits), at most 8 running at once, history kept across restarts.
- `x-eay-run-id` header to follow your own request on `/gateway/admin/events`.
- MCP: `POST /gateway/admin/mcp/import` (Claude Desktop / Claude Code / Cursor / VS Code configs), `POST /gateway/admin/mcp/:name/call` to run one tool directly.
- `--console-origin` / `EAY_GATEWAY_CONSOLE_ORIGINS`: browser origins allowed to call the gateway (CORS, including Chrome's Private Network Access preflight). `http://localhost:<port>` is always allowed.
- End-to-end tests against a mock upstream and mock MCP servers.

### Changed
- **The web console is no longer embedded.** It moved to [eay-console](https://github.com/nannndev/eay-console), a separate app that calls the gateway's API. `GET /` returns a JSON description and `/ui` is gone.
- Browser requests from origins that are not allowed now get `403` (requests without an `Origin`, like CLIs and SDKs, are unaffected).
- `/v1/responses` streams real Responses events (text, function calls, usage) and forwards function tools, `tool_choice` and `function_call` history, so Codex CLI tool use works through the gateway.
- Local MCP servers stay running between calls (stateful servers keep their state); remote ones reuse their `mcp-session-id`. Tool lists are cached.
- MCP tool names over 64 characters are shortened to a stable form; image results reach the model as images; errors include the server's stderr.
- `rate_limit_per_min` counts client requests, not failover attempts.
- Request body limit raised from 2 MB to 64 MB. Upstreams silent for 300 s are cut off; connect timeout 10 s.
- `~` in MCP commands and arguments expands to the home folder.

### Fixed
- Anthropic streams sent text deltas with index 0 even when a thinking or tool block came first.
- `/v1/responses` dropped Codex CLI's `reasoning.effort`.
- Session memory is now saved for streamed replies (it only worked for non-streaming requests).

### Upgrade notes
- The console is no longer at `/ui`. Run [eay-console](https://github.com/nannndev/eay-console) instead; a hosted copy needs `--console-origin <its origin>`.
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
