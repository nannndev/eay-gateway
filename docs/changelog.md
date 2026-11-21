# Changelog

All notable changes to **eay-gateway** are listed below.

## v0.6.0 — 2026-11-21

- **Project folders:** point a project at a folder and its agents can list, read, search and edit
  files there, and nowhere else. Pick the folder from a browser with `GET /gateway/fs/dirs`, and
  show the project's files with `GET /gateway/projects/:id/files`.
- **Rename sessions:** `PATCH /gateway/sessions/:id` gives a session a title.

## v0.5.0 — 2026-11-19

- **Projects:** group keys, teams, sessions and usage under one id (`prj_…`), with shared
  instructions for every agent and free-form metadata for your app. Send `x-eay-project`, or link
  a key so everything it does lands in its project. Usage per project, by model, agent and team.
- Releases no longer stall on GitHub's macOS runners: both macOS builds share one runner, and
  publishing runs on Linux. This is what held up the v0.4.0 release files.

## v0.4.0 — 2026-11-19

- **Agent teams:** a lead agent hands work to your saved agents (reviewer, tester, …), several at
  once. The lead is the root of the run tree. Cancelling it stops the workers under it. A saved
  agent delegates only when its own policy sets a subagent budget. The lead and its workers share
  one team thread. An agent can list the MCP servers it uses from this machine. A roster
  (`x-eay-team`, id `tm_…`) limits which agents that thread may call. Sessions store the team id
  and keep separate chats, so a rename does not drop them. Cancelling the lead skips its wrap-up
  turn.
- Usage rows now store the agent (or a worker's role) and the roster id. `GET /gateway/analytics`
  lists `agents` and `teams` with the upstream models each one used. Older rows stay out of those lists.
- Management routes dropped the `/admin` segment. `/gateway/admin/teams` is now
  `/gateway/teams`, and the same for accounts, providers, keys, agents, sessions, and the rest.
  `/v1`, `/gateway/v1`, and `/gateway/health` are unchanged. The admin token is unchanged.
  The old `/gateway/admin/*` paths still work, so existing clients such as OpenNotchi keep running.

## v0.3.1 — 2026-11-19

- **Linux binaries run everywhere:** static builds (no glibc or OpenSSL needed) for x64 and the new
  ARM64, so they work on Ubuntu 20.04+, Debian, Alpine and Raspberry Pi.

## v0.3.0 — 2026-11-19

### Highlights
- **Agents that delegate:** a model can start its own subagents (`max_subagents`), with manual approval for tools that change things, and a sessions API to see what happened.
- **Thinking everywhere:** reasoning effort and Claude thinking are translated for every provider; reasoning streams back to clients.
- **Tool policies:** limit which MCP tools the gateway runs per agent or key (`read_only`, allow/deny lists).
- **Easy install:** Homebrew, Docker image, and `eay-gateway service install` to start at login; new [Integrations](integrations.md) guide.
- **Safer by default:** admin API and MCP tools are loopback-only without an admin token (blocks DNS rebinding); new read-only admin token; session memory is private per key.
- **Codex CLI works end to end:** `/v1/responses` streams real Responses events and carries function tools and tool-call history.
- **Multi-agent:** cancel, join (`?wait=`), parent trees, MCP access and per-key limits for subagents; at most 8 at once; history survives restarts.
- **Multi-access:** per-key `max_concurrent`; rate limits count requests, not failover attempts.
- **MCP:** servers stay running between calls, config import from other apps, run a tool directly, long tool names shortened, image results passed as images, stderr in errors.
- **Limits:** 64 MB request bodies, 300 s upstream silence cut-off.
- **Headless:** the web console moved to its own app, [eay-console](https://github.com/nannndev/eay-console); the gateway allows it through `--console-origin` and refuses other web pages.

Upgrade notes: the console moved out of `/ui` to [eay-console](https://github.com/nannndev/eay-console); older sessions are not loaded (they were not tied to a key); reaching admin through a non-loopback hostname without `--admin-token` now returns `403`.

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
