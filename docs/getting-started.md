# Getting started

## Install

**Homebrew** (macOS, Linux x64):

```sh
brew install nannndev/tap/eay-gateway
brew services start eay-gateway     # start now and at login
```

**Prebuilt binary:** download `eay-gateway-<platform>` from the
[releases page](https://github.com/nannndev/eay-gateway/releases) and check it against `SHA256SUMS.txt`.

| Platform | File |
|---|---|
| macOS (Apple Silicon + Intel) | `eay-gateway-universal-apple-darwin` |
| Windows x64 | `eay-gateway-x86_64-pc-windows-msvc.exe` |
| Linux x64 | `eay-gateway-x86_64-unknown-linux-musl` (also published as `…-linux-gnu`) |
| Linux ARM64 (Raspberry Pi, ARM VPS) | `eay-gateway-aarch64-unknown-linux-musl` |

Linux binaries are fully static: one file, no glibc or OpenSSL needed, so they run on any
distribution (Ubuntu 20.04+, Debian, Alpine, …).

**From source** (Rust 1.89 or newer):

```sh
git clone https://github.com/nannndev/eay-gateway && cd eay-gateway
cargo build --release --bin eay-gateway
./target/release/eay-gateway
```

It prints `eay-gateway v0.3.0 listening on http://127.0.0.1:20129 (data: ~/.eay-gateway)`.
Check it with `curl http://127.0.0.1:20129/health`.

### Start at login

```sh
eay-gateway service install            # add gateway options after `install` if you need them
eay-gateway service status
eay-gateway service uninstall
```

This is per user and needs no admin rights: a LaunchAgent on macOS (log in
`~/Library/Logs/eay-gateway.log`), a systemd user unit on Linux (`journalctl --user -u eay-gateway`),
and a scheduled task at logon on Windows. Running `install` again replaces the old setup. With
Homebrew, use `brew services` instead.

## Connect an API key

```sh
curl -X POST http://127.0.0.1:20129/gateway/providers/openai/connect \
  -H 'content-type: application/json' -d '{"api_key":"sk-..."}'
```

The gateway checks the key against the provider first and fetches its model list. Some providers
need a base URL, for example Azure OpenAI or Cloudflare:

```sh
curl -X POST http://127.0.0.1:20129/gateway/providers/azure-openai/connect \
  -H 'content-type: application/json' \
  -d '{"api_key":"...","base_url":"https://<resource>.openai.azure.com/openai/v1"}'
```

Add more keys for the same provider with `POST /gateway/providers/<id>/keys`. They rotate
together (see [Routing](routing.md)).

## Log in with a subscription

Subscription providers use the provider's own login:

```sh
curl -X POST http://127.0.0.1:20129/gateway/accounts/claude/start -H 'content-type: application/json' -d '{}'
```

- **Browser login** (`"kind":"redirect"`): open `auth_url`. The gateway listens on a local
  callback port and finishes the login by itself.
- **Device login** (`"kind":"device"`): open `verification_uri` and enter `user_code`, then poll
  `POST /gateway/accounts/<provider>/poll` with `{"session_id": "..."}` until it returns
  `{"status":"done"}`.

Some logins can be imported from a tool already signed in on this machine: the Codex CLI,
the Kiro IDE, the Command Code CLI and MiMo Desktop. See [Providers](providers.md).

## On a server, NAS or Raspberry Pi (Docker)

On your own computer, use Homebrew or the binary above. Docker fits a gateway that runs on a home
server, NAS or VPS and is shared by your machines or a team. On a single machine the static Linux
binary plus `eay-gateway service install` does the same job with less setup.

```sh
docker run -d --name eay-gateway --restart unless-stopped \
  -p 127.0.0.1:20129:20129 -v eay-data:/data \
  -e EAY_GATEWAY_ADMIN_TOKEN=change-me ghcr.io/nannndev/eay-gateway
```

Or use `compose.yaml` from the repo with `EAY_GATEWAY_ADMIN_TOKEN` in a `.env` file. The image is
`linux/amd64` and `linux/arm64`; data lives in the `/data` volume.

What is different in a container:

- **Admin token is required.** Nothing inside a container looks like localhost, so
  management routes (`/gateway/accounts`, `/gateway/keys`, and the rest) refuse requests without `EAY_GATEWAY_ADMIN_TOKEN`. Send it as
  `Authorization: Bearer …` or `x-admin-token`.
- **Browser logins need one extra step.** The login page sends you back to `http://localhost:<port>`
  on your computer, not into the container. When the page fails to load, copy that URL from the
  address bar and finish with `POST /gateway/accounts/<provider>/complete`
  (`{"session_id":"…","callback":"<the URL>"}`), or paste it into eay-console. Device logins
  (enter a code on a website) work as usual.
- **Local MCP servers don't run.** The image has no Node or Python, so `npx`/`uvx` servers fail.
  Use remote (`http`) MCP servers, or run the gateway natively.
- **Reaching it from other machines:** publish the port on all interfaces (`-p 20129:20129`) only
  behind your firewall or a reverse proxy with TLS. Give each person a [virtual key](api.md#virtual-keys)
  with its own limits.

## Use it from your tools

[Integrations](integrations.md) has setup for Claude Code, Codex, Cursor, Cline, SDKs, desktop
and web apps. The short version:

Any OpenAI-compatible client works. Use base URL `http://127.0.0.1:20129/v1` and any API key,
unless you set up [virtual keys](api.md#virtual-keys).

```sh
# OpenAI SDKs and most CLIs
export OPENAI_BASE_URL=http://127.0.0.1:20129/v1
export OPENAI_API_KEY=anything
```

```python
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:20129/v1", api_key="anything")
print(client.chat.completions.create(model="auto", messages=[{"role": "user", "content": "Hi"}]).choices[0].message.content)
```

Pick a model with the [model id](routing.md#model-ids): `auto`, `cc/auto`,
`cc/claude-sonnet-5-5`, `auto/<combo>`. `GET /v1/models` lists what your connected providers
offer right now.

## Next

- [Integrations](integrations.md): set up Claude Code, Codex, Cursor, SDKs and apps
- [Routing](routing.md): control which account handles a request
- [HTTP API](api.md): agents, tool policies, subagents, approvals and sessions
- [Configuration](configuration.md): run it for several apps at once
