# Provider Support — Design Doc

**Status:** Draft (leader proposal, 2026-10-20; research: two scout reports commissioned by ZC Leader)
**Scope:** which API providers ZenCode ships built-in presets for, and the rules `zen provider add` follows.
**Foundation:** Constitution VIII — **BYOK**: ZenCode ships no API keys. Users add their own keys; every model call runs on the user's key/endpoint (or local). Research records: `zencode-provider-*-research-2026-10-20.md` (team workspace).

## 1. Preset tiers

### Tier 1 — direct providers (built-in for v1.0.0)

| Preset | Why | Notes |
|---|---|---|
| `anthropic` | Best agent quality; cheapest prompt cache for long loops; Messages API native | Warn on ToS-suspend history |
| `openai` | Reference standard; most users already hold a key | Responses + Chat Completions |
| `deepseek` | Frontier quality at the lowest price; supports both API formats | **Strongest ban/rate-limit risk — warn + automatic backoff required** |
| `google` (AI Studio) | Real free tier, no card — the free-first on-ramp | Free tier may train on data — warn; paid tier does not |

### Tier 1 — gateways & local

| Preset | Why | Notes |
|---|---|---|
| `openrouter` | **Recommended default for beginners**: one key → 500+ models incl. Claude/GPT; free models; Anthropic Messages; best-in-class zero-retention routing | 5.5% credit top-up fee — disclosed in docs |
| `groq` | Free tier + fastest first-token for agent loops | Open-weight catalog only |
| `cerebras` | Free 1M tokens/day, highest throughput | Narrow catalog |
| `ollama` | Local default — fully offline, no key at all | OpenAI-compatible at `localhost:11434/v1` |
| `litellm` | Self-hosted gateway preset for power users (keys never leave user infra) | User operates it (Postgres/Redis) |
| `9router` | Local router (MIT, decolua/9router): one endpoint → 40+ providers, 3-tier auto-fallback, token compression | Local key from its dashboard; **subscription-routing ban-risk warning required** |
| `omniroute` | Local-first gateway (MIT, diegosouzapw/OmniRoute): one OpenAI-compatible endpoint → 230+ providers, 17 routing strategies, built-in MCP | Same warning class as `9router` |

### Tier 2 — nice-to-have (post-v1.0.0 or if capacity allows)

Direct: `qwen` (cheap, strong coder; China-law data warning), `mistral` (EU residency, Codestral), `glm` (agent-first, zero-retention PAYG), `xai` (warn: silent model redirects can change billing), `moonshot` (strong coding; mainland KYC friction — point to intl USD endpoint).
Gateways: `fireworks` (Anthropic passthrough, strong tool use), `together`, `requesty` (EU option), `novita` / `siliconflow` (cheap CN models, **data-policy warning label required**).
Local: `lm-studio`, `llama.cpp`, `vllm` presets for advanced local setups.

### Skipped

`cohere` (enterprise RAG focus, weak for coding agents), `meta-llama` (no first-party API — covered by **custom endpoint**), `portkey` (enterprise, keys held vendor-side — wrong segment), `zenmux` (enterprise positioning, low transparency).

### Custom endpoint

Any OpenAI-compatible base URL can be added without a preset (`zen provider add custom --base-url …`). Presets are convenience, never a whitelist.

## 2. `zen provider add` rules

1. **BYOK only.** Presets contain base URLs and metadata — never keys. Keys go to the OS keyring (see secret-store design); config files store references, never raw keys.
2. **Account-risk warning** (standing product rule): adding `deepseek`, `anthropic`, `google`, `xai`, or a China-law provider prints a one-screen warning: ban/rate-limit history, data-policy summary, and the advice to pin model versions.
3. **Data-policy label** per preset in `zen provider list`: `trains-on-data: no | free-tier-only | china-law`, `retention: …` — one line, plain language.
4. **Pin model versions.** Provider model IDs are stored pinned; silent upstream redirects must surface as a notice, not a surprise bill.
5. **Automatic backoff** on 429/403 with jitter; never hammer a provider into banning the user's key.
6. **Multi-model review** (design: Việc 6) composes only providers the user has added — no team key exists to fall back on, by design.

## 3. Recommendation flow for new users

`zen` first run suggests, in order: **Ollama** (offline, $0) → **Google AI Studio free tier** (cloud, $0, data warning) → **OpenRouter** (one key, everything) → direct **Anthropic/OpenAI/DeepSeek** keys for best price/quality per model. Each step is skippable; nothing is pre-enabled.

## 4. ZC↔ZD unified contract (agreed 2026-10-20, relay)

**Canonical preset list** (ZD Leader's 3 tiers adopted as the shared standard; ids in backticks):
- Recommended: `openrouter`, `deepseek`, `anthropic`, `openai`, `gemini`
- Advanced: `together`, `fireworks`, `groq`, `cerebras`, `xai`, `mistral`, `moonshot`, `zhipu`, `dashscope`, `custom`
- Local: `ollama`, `lm-studio`
- Next list revision candidates: `litellm`, `requesty`, `novita`, `siliconflow`, `llama.cpp`, `vllm`.
- Added by owner instruction (2026-10-20, ZD sync pending): local router presets `9router` (base URL `http://localhost:20128/v1`, adjustable) and `omniroute` (base URL `http://localhost:20128/v1`, adjustable). Both are user-operated local gateways — ZenCode stores only the local key in the keyring (BYOK holds). Because both can route subscription/OAuth accounts, their presets carry the account-risk warning: routing subscription accounts through third-party routers may violate provider terms and risks account suspension.

**Key passing:** Desktop stores user keys in the OS secret store and passes them to the `zen` child process via environment variables. CLI reads, in precedence order: environment → OS keyring → config reference. Canonical env names: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, `DEEPSEEK_API_KEY`, `GEMINI_API_KEY` (alias `GOOGLE_API_KEY`), `GROQ_API_KEY`, `CEREBRAS_API_KEY`, `TOGETHER_API_KEY`, `FIREWORKS_API_KEY`, `XAI_API_KEY`, `MISTRAL_API_KEY`, `MOONSHOT_API_KEY`, `ZHIPU_API_KEY`, `DASHSCOPE_API_KEY`; generic override `ZEN_BASE_URL` + `ZEN_API_KEY`. Keys are never written to files or logs.

**Model registry:** `~/.zencode/models.json`, schema v1: `{version, providers: {id: {base_url, env_key, format ∈ openai|anthropic}}, models: [{id, provider, name, context, free}], explicit_free: [ids]}`. Maintained by the CLI side (agreement D7). Schema changes require a version bump and advance notice to the other team.

**Shared privacy labels** (verbatim in CLI list output and Desktop Settings): "Does not train on API data" · "Free tier may use your data" · "Subject to China data law" · "Zero-retention routing available" · "Runs locally — nothing leaves your machine". Account-risk warnings shared verbatim for: deepseek, anthropic, gemini (free tier), xai.
