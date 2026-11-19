# Configuration

## Command line and environment

| Flag | Environment | Default | |
|---|---|---|---|
| `--host <HOST>` | `EAY_GATEWAY_HOST` | `127.0.0.1` | Keep it on localhost; the admin API has no auth |
| `--port <PORT>` | `EAY_GATEWAY_PORT` | `20129` | |
| `--data-dir <PATH>` | `EAY_GATEWAY_DATA_DIR` | see below | |
| `--admin-token <T>` | `EAY_GATEWAY_ADMIN_TOKEN` | off | Full admin access token |
| `--admin-read-token <T>` | `EAY_GATEWAY_ADMIN_READ_TOKEN` | off | Admin token that can only `GET` |
| `--console-origin <O>` | `EAY_GATEWAY_CONSOLE_ORIGINS` | off | Browser origin(s), comma-separated, allowed to call the gateway (see below) |
| `--idle-exit <MIN>` | | off | Exit after MIN minutes with no HTTP requests |
| `-V`, `--version` | | | Print the version |
| `service install\|uninstall\|status` | | | Start at login (see [Getting started](getting-started.md#start-at-login)) |
| `-h`, `--help` | | | |

## Web console and browser access

The gateway has no web UI of its own; `GET /` returns a short JSON description. The web console is
a separate app, [eay-console](https://github.com/nannndev/eay-console), that calls the gateway's API
from the browser.

Browsers send an `Origin` header, and the gateway only answers origins it trusts:

- `http://localhost:<port>`, `http://127.0.0.1:<port>` and `http://[::1]:<port>` are always allowed,
  so a console running locally works with no setup.
- A hosted console needs its origin listed: `--console-origin https://nannndev.github.io`.
- Any other web page gets `403`, including simple form posts and uploads, so a site you visit
  cannot use your gateway.
- Requests without an `Origin` (CLIs, SDKs, desktop apps calling from native code) are not affected.

Allowed preflights also answer Chrome's Private Network Access check, which a public page needs
before it can call `127.0.0.1`. If you expose the gateway beyond localhost, set `--admin-token` too.

The older `NOTCHI_GATEWAY_HOST`, `NOTCHI_GATEWAY_PORT` and `NOTCHI_GATEWAY_DATA_DIR` names are
still read when the `EAY_GATEWAY_*` ones are unset.

## Data folder

| OS | Default |
|---|---|
| macOS, Linux | `~/.eay-gateway` |
| Windows | `%APPDATA%\eay-gateway` |

| File | Contents |
|---|---|
| `gateway.db` | SQLite: providers, accounts, extra keys, usage log, settings, combos, virtual keys |
| `gateway.key` | 32-byte key that encrypts secrets in `gateway.db` (AES-256-GCM); `0600` on Unix |
| `gateway.lock` | Held while a gateway runs on this folder |

Back up `gateway.db` and `gateway.key` together. One without the other is useless.

**Migration:** on first run with the default folder, the gateway moves an existing
`~/.notchi-gateway` (or `%APPDATA%\notchi-gateway`) to the new location, so accounts survive the
rename from Notchi Gateway.

## Sharing one gateway between apps

Several apps on one machine (for example OpenNotchi plus your own scripts) should share one gateway,
not each run their own:

1. **Probe** `GET /health`. If it answers `{"service":"eay-gateway","version":"…"}` and the version
   is new enough for you, use it.
2. **Otherwise start it** with `--idle-exit 30`, and don't kill it when your app quits: another app
   may be using it. It exits by itself after 30 minutes without requests.
3. **One gateway per data folder.** A second gateway on the same folder exits with status **3**
   ("another eay-gateway is already using …"). This protects OAuth accounts: two gateways
   refreshing the same token would invalidate each other's refresh tokens.

To run a separate, isolated gateway on purpose, give it its own `--port` and `--data-dir`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Stopped normally (signal or `--idle-exit`) |
| 1 | Couldn't open the database, bind the port, or the server failed |
| 2 | Bad command-line arguments or bind address |
| 3 | Another gateway already uses this data folder |

## Background work

- **Model lists:** every hour the gateway re-fetches the model list of each connected provider whose
  list is more than a day old.
- **Quotas:** fetched on demand by `GET /gateway/admin/accounts/usage` and cached for 2 minutes per
  account. A failed read keeps the last good values.
- **Tokens:** OAuth access tokens are refreshed shortly before they expire, and once more on a 401.

## Limits

| Limit | Value | What happens |
|---|---|---|
| Request body | 64 MB | Larger bodies get `413`. Fits long agent contexts, inline images/PDFs and audio uploads. |
| Upstream connect | 10 s | The next provider is tried. |
| Upstream silence | 300 s | A provider that sends nothing for this long (mid-stream too) is cut off; long generations that keep streaming are not affected. |
| Concurrent subagents | 8 | Extra subagents wait in `queued`. |
| Per-key requests in flight | `max_concurrent` on the key | Over the cap → `429`. |
| MCP `tools/call` | 120 s | The tool returns an error to the model. |
| Idle local MCP server | 10 min | The process exits; the next call starts it again. |
