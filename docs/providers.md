# Providers

Model ids are `<alias>/<model>`. The **alias** column below is what goes before the `/`.

## Subscription providers (OAuth)

Connect with `POST /gateway/admin/accounts/<id>/start` (see [Getting started](getting-started.md#log-in-with-a-subscription)).

| Provider | Id | Alias | Login | Live quota |
|---|---|---|---|---|
| Claude (Pro / Max) | `claude` | `cc` | Browser | 5-hour and weekly windows |
| ChatGPT / Codex (Plus / Pro) | `codex` | `cx` | Browser, or import from Codex CLI | Session and weekly windows |
| GitHub Copilot | `copilot` | `gh` | Device code | Chat, completions, premium requests |
| Gemini CLI (Google) | `gemini-cli` | `gc` | Browser, with your own Google OAuth client | Per model |
| Kimi Code | `kimi` | `km` | Device code | Weekly and rate limit |
| Muse Code (Meta) | `muse` | `mu` | Device code | – |
| Nous Research | `nous` | `ns` | Device code | – |
| Kilo Code | `kilocode` | `kc` | Device code | – |
| Cline | `cline` | `cl` | Browser | 5-hour, weekly, monthly |
| MiniMax Code (China) ⚠ | `minimax-code` | `mmc` | Device code | – |
| MiniMax Code (Global) ⚠ | `minimax-code-global` | `mmcg` | Device code | – |
| Antigravity ⚠ | `antigravity` | `ag` | Browser | Gemini and Claude & GPT windows |
| Grok CLI ⚠ | `grok-cli` | `gcli` | Device code | Monthly and weekly credits |
| Kiro ⚠ | `kiro` | `kr` | AWS Builder ID device code, or import from Kiro IDE | Per resource |

⚠ Experimental: these impersonate the official client. See the README.

**Gemini CLI** needs your own Google OAuth client (Desktop type). Save it once with
`POST /gateway/admin/settings` `{"gemini_oauth_client_id":"…","gemini_oauth_client_secret":"…"}`.

**Imports** read a login the official tool already saved, including its refresh token:

| Provider | Endpoint | Reads |
|---|---|---|
| Codex | `POST /gateway/admin/providers/codex/import` | `~/.codex/auth.json` (or `$CODEX_HOME`) |
| Kiro | `POST /gateway/admin/providers/kiro/import` | `~/.aws/sso/cache/` and the Kiro profile |

## API-key providers

Connect with `POST /gateway/admin/providers/<id>/connect` `{"api_key":"…","base_url":"…"}`.
Add more keys with `POST /gateway/admin/providers/<id>/keys` `{"api_key":"…"}`.

| Provider | Id | Alias | Notes |
|---|---|---|---|
| OpenAI | `openai` | `oa` | |
| Anthropic | `anthropic` | `an` | Native Messages API |
| DeepSeek | `deepseek` | `ds` | |
| Google Gemini | `gemini` | `gm` | OpenAI-compatible endpoint |
| OpenRouter | `openrouter` | `or` | |
| Groq | `groq` | `gq` | |
| Mistral AI | `mistral` | `ms` | |
| Together AI | `together` | `tg` | |
| xAI (Grok) | `xai` | `xa` | |
| Cohere | `cohere` | `co` | Compatibility API |
| Fireworks AI | `fireworks` | `fw` | |
| Perplexity | `perplexity` | `pp` | |
| Perplexity Agent | `perplexity-agent` | `pa` | Responses API |
| Cerebras | `cerebras` | `cb` | |
| Hyperbolic | `hyperbolic` | `hb` | |
| SambaNova | `sambanova` | `sn` | |
| Novita AI | `novita` | `nv` | |
| SiliconFlow | `siliconflow` | `sf` | |
| GitHub Models | `github` | `gh_api` | GitHub token |
| Ollama (local) | `ollama` | `ol` | No key needed |
| Azure OpenAI | `azure-openai` | `az` | v1 API; base URL required: `https://<resource>.openai.azure.com/openai/v1` |
| Moonshot Kimi | `moonshot` | `moonshot` | |
| Alibaba Qwen (DashScope) | `dashscope` | `dashscope` | |
| Z.ai GLM | `zai` | `zai` | |
| GLM Coding (China) | `glm-cn` | `glm-cn` | |
| MiniMax | `minimax` | `minimax` | |
| MiniMax (China) | `minimax-cn` | `minimax-cn` | |
| NVIDIA NIM | `nvidia` | `nvidia` | |
| DeepInfra | `deepinfra` | `deepinfra` | |
| Nebius AI Studio | `nebius` | `nebius` | |
| Venice AI | `venice` | `vn` | |
| Chutes | `chutes` | `ch` | |
| Featherless | `featherless` | `fl` | |
| Hugging Face Router | `huggingface` | `hf` | |
| Vercel AI Gateway | `vercel-ai-gateway` | `vercel` | |
| Blackbox AI | `blackbox` | `bb` | |
| Poolside | `poolside` | `ps` | |
| Runcrate | `runcrate` | `rc` | |
| TokenHarbor | `tokenharbor` | `th` | |
| Agnes AI | `agnes` | `agnes` | |
| Atria | `atria` | `atria` | |
| Dahl Inference | `dahl` | `dahl` | |
| OpenCode Go | `opencode-go` | `ocg` | |
| OpenCode Zen | `opencode-zen` | `ocz` | |
| Kimchi | `kimchi` | `kimchi` | Free tier |
| Volcengine Ark (Coding) | `volcengine-ark` | `ark` | |
| BytePlus ModelArk (Coding) | `byteplus` | `bpm` | |
| Xiaomi MiMo | `xiaomi-mimo` | `mimo` | Or import from MiMo Desktop |
| Xiaomi MiMo Token Plan | `xiaomi-tokenplan` | `xmtp` | |
| Cloudflare Workers AI | `cloudflare-ai` | `cf` | Base URL required: `https://api.cloudflare.com/client/v4/accounts/<account-id>/ai/v1` |
| Command Code | `commandcode` | `cmc` | Native generate API; or import from `~/.commandcode/auth.json` |
| Custom | `custom` | `custom` | Any OpenAI-compatible server; base URL required |

## Models

Each provider starts with a short built-in list. Once connected, the gateway fetches the provider's
own list (`/models`, or the provider's equivalent) and refreshes it daily, or on demand with
`POST /gateway/admin/providers/<id>/models/sync`. Subscription providers with a model API
(Copilot, Kimi, Kilo Code, Nous, Cline, Grok CLI, Antigravity) sync the same way.

Add models the provider doesn't list with `POST /gateway/admin/providers/<id>/models`
`{"model":"…"}`. Custom models are tried first in `auto`.

## Cost

Requests log input, output and cached tokens, plus an estimated cost from a built-in price table
(what the request would cost on the provider's API). Subscriptions don't bill per token, so treat
it as a usage comparison, not an invoice. Models missing from the table count as $0.
