# Routing

Every chat request names a model. The gateway expands it into an ordered list of attempts
(provider, model, account or key) and tries them until one succeeds.

## Model ids

| Model id | Tries |
|---|---|
| `auto` | Every connected provider in your priority order, with each provider's preferred models |
| `<alias>/auto` | One provider, e.g. `cx/auto` for ChatGPT/Codex or `ds/auto` for DeepSeek |
| `auto/<alias>` | Same as `<alias>/auto` |
| `auto/<combo>` | Your own ordered list of targets (see [Combos](#combos)) |
| `<alias>/<model>` | One model, e.g. `cc/claude-sonnet-5-5`. For subscription providers it falls back to that provider's other preferred models |

Aliases are listed in [Providers](providers.md). A provider's full id works too (`claude/…`).

`GET /v1/models` lists the auto modes first (`auto`, your combos, `<alias>/auto` per connected
provider), then the models of connected providers only, in priority order. `owned_by` is the
provider id, so clients can group the list.

## What `auto` tries

1. **Provider order:** your saved priority first, then any other connected subscription providers,
   then connected API-key providers.
2. **Models per provider:**
   - *Subscription providers* use a built-in preferred list, which is shorter for free plans.
   - *API-key providers* try your custom models first, then the provider's list, capped at
     **Models per API-key provider** (default 2) so a dead key doesn't cost dozens of attempts.

## Combos

A combo is a named, ordered list of targets. Each entry is a model or `<alias>/auto`:

```sh
curl -X POST http://127.0.0.1:20129/gateway/admin/combos -H 'content-type: application/json' \
  -d '{"name":"coding","targets":["cx/gpt-5.5","cc/auto","ds/deepseek-chat"]}'
```

Use it as `auto/coding`. Names are one word with no `/` or spaces. Duplicate targets are dropped.

## Accounts and keys

A provider can have several accounts (subscriptions) or several keys (API-key providers: the main key
plus extra keys). They're tried in your order (`POST …/accounts/<provider>/reorder`), adjusted by
the strategy:

| Strategy | Behaviour |
|---|---|
| `priority` (default) | Always start with the first account; fall back in order |
| `round_robin` | Each request starts with the next account, spreading load |
| `fill_first` | Keep using the account that last succeeded until it fails or hits a limit |

**Fallback order** decides how models and accounts combine:

| Fallback order | Order of attempts for models M1, M2 on accounts A, B |
|---|---|
| `model_first` (default) | M1·A, M1·B, M2·A, M2·B: the best model on every account first |
| `account_first` | M1·A, M2·A, M1·B, M2·B: every model on one account first |

Paused accounts and keys are skipped.

## Cooldown

When an upstream answers **429** or a body mentions a rate limit, quota or usage limit, that
account or key is benched:

- for `Retry-After` seconds, or the provider's `resets_in_seconds`, when given;
- otherwise for **Cooldown** seconds (default 60; `0` disables the default).

Benched accounts move to the end of the attempt list rather than being skipped, so they remain a
last resort. Cooldowns are kept in memory and reset when the gateway restarts.

Other failures (5xx, unknown model, network errors) move straight to the next attempt without
a cooldown.

## Settings

`POST /gateway/admin/settings` with any of:

| Field | Values | Default |
|---|---|---|
| `routing_strategy` | `priority`, `round_robin`, `fill_first` | `priority` |
| `fallback_order` | `model_first`, `account_first` | `model_first` |
| `provider_priority` | provider ids, e.g. `["codex","claude","deepseek"]` | `[]` |
| `auto_models_per_provider` | 1–10 | 2 |
| `cooldown_secs` | 0–3600 | 60 |

`GET /gateway/admin/settings` also returns `auto_order`: the provider order `auto` uses right now.
