// Companion LLM client — lets the DCENT_OS companion chat with the operator and
// take miner actions ("Hey, lower the noise of my miner" → it does it).
//
// PRIVACY (load-bearing, matches the no-phone-home firmware): this is OFF BY
// DEFAULT and points at a LOCAL LLM by default (Ollama / LM Studio). Cloud
// providers (OpenAI / Anthropic / OpenRouter) are supported but explicitly NOT
// recommended — they send your prompts off-box. The API key + config live only
// in this browser's localStorage; nothing is sent anywhere until the operator
// enables it and picks an endpoint.

export type LlmProvider = 'ollama' | 'lmstudio' | 'openai' | 'anthropic' | 'openrouter';

export interface LlmConfig {
  enabled: boolean;
  provider: LlmProvider;
  baseUrl: string;
  model: string;
  apiKey: string;
}

/** Provider presets — base URL + a sensible default model + whether it's local. */
export const PROVIDER_PRESETS: Record<LlmProvider, { label: string; baseUrl: string; model: string; local: boolean; needsKey: boolean }> = {
  ollama:     { label: 'Ollama (local)',      baseUrl: 'http://localhost:11434/v1', model: 'llama3.2',            local: true,  needsKey: false },
  lmstudio:   { label: 'LM Studio (local)',   baseUrl: 'http://localhost:1234/v1',  model: 'local-model',        local: true,  needsKey: false },
  openai:     { label: 'OpenAI (cloud)',      baseUrl: 'https://api.openai.com/v1', model: 'gpt-4o-mini',        local: false, needsKey: true  },
  anthropic:  { label: 'Anthropic (cloud)',   baseUrl: 'https://api.anthropic.com', model: 'claude-haiku-4-5',   local: false, needsKey: true  },
  openrouter: { label: 'OpenRouter (cloud)',  baseUrl: 'https://openrouter.ai/api/v1', model: 'meta-llama/llama-3.1-8b-instruct', local: false, needsKey: true },
};

export const DEFAULT_LLM_CONFIG: LlmConfig = {
  enabled: false,
  provider: 'ollama',
  baseUrl: PROVIDER_PRESETS.ollama.baseUrl,
  model: PROVIDER_PRESETS.ollama.model,
  apiKey: '',
};

const STORAGE_KEY = 'dcentos-companion-llm-v1';

export function loadLlmConfig(): LlmConfig {
  if (typeof window === 'undefined') return { ...DEFAULT_LLM_CONFIG };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_LLM_CONFIG };
    const p = JSON.parse(raw) as Partial<LlmConfig>;
    return {
      enabled: Boolean(p.enabled),
      provider: (p.provider && p.provider in PROVIDER_PRESETS ? p.provider : 'ollama') as LlmProvider,
      baseUrl: typeof p.baseUrl === 'string' && p.baseUrl ? p.baseUrl : DEFAULT_LLM_CONFIG.baseUrl,
      model: typeof p.model === 'string' && p.model ? p.model : DEFAULT_LLM_CONFIG.model,
      apiKey: typeof p.apiKey === 'string' ? p.apiKey : '',
    };
  } catch {
    return { ...DEFAULT_LLM_CONFIG };
  }
}

export function saveLlmConfig(cfg: LlmConfig): void {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
  } catch {
    // Config is cosmetic-tier; never throw into the dashboard if storage is full.
  }
}

export function isCloud(provider: LlmProvider): boolean {
  return !PROVIDER_PRESETS[provider].local;
}

// ── Chat wire types (provider-neutral) ──────────────────────────────────────
export interface ToolSpec {
  name: string;
  description: string;
  parameters: Record<string, unknown>; // JSON Schema
}
export interface ToolCall {
  id: string;
  name: string;
  arguments: Record<string, unknown>;
}
export interface ChatTurn {
  role: 'system' | 'user' | 'assistant' | 'tool';
  content: string;
  toolCalls?: ToolCall[];
  toolCallId?: string; // for role: 'tool'
}
export interface ChatResult {
  content: string;
  toolCalls: ToolCall[];
}

/** Send a chat turn to the configured provider and return the assistant reply
 *  (text + any tool calls). Throws a friendly Error on misconfig / network. */
export async function chat(cfg: LlmConfig, turns: ChatTurn[], tools: ToolSpec[]): Promise<ChatResult> {
  if (!cfg.enabled) throw new Error('Companion chat is disabled — enable an LLM in Settings.');
  if (!cfg.baseUrl) throw new Error('No LLM endpoint configured.');
  if (PROVIDER_PRESETS[cfg.provider].needsKey && !cfg.apiKey) {
    throw new Error(`${PROVIDER_PRESETS[cfg.provider].label} needs an API key (Settings → Companion).`);
  }
  return cfg.provider === 'anthropic'
    ? chatAnthropic(cfg, turns, tools)
    : chatOpenAiCompatible(cfg, turns, tools);
}

// Build the chat-completions URL robustly. Users often enter the host WITHOUT
// the OpenAI `/v1` segment (e.g. `http://127.0.0.1:1234`), which makes LM Studio
// answer "Unexpected endpoint (OPTIONS /chat/completions)" and the request fails.
// Normalize so `…:1234`, `…:1234/v1`, and a full `…/chat/completions` all work.
function completionsUrl(baseUrl: string): string {
  let u = baseUrl.trim().replace(/\/+$/, '');
  if (/\/chat\/completions$/.test(u)) return u;
  if (!/\/v\d+$/.test(u)) u += '/v1';
  return u + '/chat/completions';
}

// ── OpenAI-compatible (Ollama / LM Studio / OpenAI / OpenRouter) ─────────────
async function chatOpenAiCompatible(cfg: LlmConfig, turns: ChatTurn[], tools: ToolSpec[]): Promise<ChatResult> {
  const messages = turns.map(t => {
    if (t.role === 'tool') return { role: 'tool', tool_call_id: t.toolCallId, content: t.content };
    if (t.role === 'assistant' && t.toolCalls?.length) {
      return {
        role: 'assistant',
        content: t.content || null,
        tool_calls: t.toolCalls.map(c => ({ id: c.id, type: 'function', function: { name: c.name, arguments: JSON.stringify(c.arguments) } })),
      };
    }
    return { role: t.role, content: t.content };
  });
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (cfg.apiKey) headers.Authorization = `Bearer ${cfg.apiKey}`;
  const send = async (withTools: boolean): Promise<Response> => {
    const body: Record<string, unknown> = { model: cfg.model, messages, temperature: 0.3 };
    if (withTools && tools.length) {
      body.tools = tools.map(t => ({ type: 'function', function: { name: t.name, description: t.description, parameters: t.parameters } }));
      body.tool_choice = 'auto';
    }
    try {
      return await fetch(completionsUrl(cfg.baseUrl), {
        method: 'POST', headers, body: JSON.stringify(body),
      });
    } catch (e) {
      throw new Error(networkError(e, cfg)); // CORS / mixed-content / server-down
    }
  };
  let res = await send(tools.length > 0);
  // Many local models reject `tools` outright (HTTP 400/422/500) — fall back to a
  // plain chat so the user still gets a reply (it just can't take miner actions).
  if (!res.ok && tools.length && [400, 422, 500].includes(res.status)) {
    res = await send(false);
  }
  if (!res.ok) throw new Error(await friendlyError(res, cfg));
  const data = await res.json();
  const msg = data?.choices?.[0]?.message ?? {};
  const toolCalls: ToolCall[] = (msg.tool_calls ?? []).map((c: any) => ({
    id: c.id ?? c.function?.name ?? 'call',
    name: c.function?.name ?? '',
    arguments: safeParse(c.function?.arguments),
  }));
  return { content: typeof msg.content === 'string' ? msg.content : '', toolCalls };
}

// ── Anthropic Messages API ──────────────────────────────────────────────────
async function chatAnthropic(cfg: LlmConfig, turns: ChatTurn[], tools: ToolSpec[]): Promise<ChatResult> {
  const system = turns.filter(t => t.role === 'system').map(t => t.content).join('\n\n');
  const messages: any[] = [];
  for (const t of turns) {
    if (t.role === 'system') continue;
    if (t.role === 'assistant') {
      const blocks: any[] = [];
      if (t.content) blocks.push({ type: 'text', text: t.content });
      for (const c of t.toolCalls ?? []) blocks.push({ type: 'tool_use', id: c.id, name: c.name, input: c.arguments });
      messages.push({ role: 'assistant', content: blocks.length ? blocks : t.content });
    } else if (t.role === 'tool') {
      messages.push({ role: 'user', content: [{ type: 'tool_result', tool_use_id: t.toolCallId, content: t.content }] });
    } else {
      messages.push({ role: 'user', content: t.content });
    }
  }
  const body: Record<string, unknown> = { model: cfg.model, max_tokens: 1024, system, messages };
  if (tools.length) body.tools = tools.map(t => ({ name: t.name, description: t.description, input_schema: t.parameters }));
  let res: Response;
  try {
    res = await fetch(`${cfg.baseUrl.replace(/\/$/, '')}/v1/messages`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'x-api-key': cfg.apiKey,
        'anthropic-version': '2023-06-01',
        'anthropic-dangerous-direct-browser-access': 'true',
      },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(networkError(e, cfg));
  }
  if (!res.ok) throw new Error(await friendlyError(res, cfg));
  const data = await res.json();
  let content = '';
  const toolCalls: ToolCall[] = [];
  for (const block of data?.content ?? []) {
    if (block.type === 'text') content += block.text;
    else if (block.type === 'tool_use') toolCalls.push({ id: block.id, name: block.name, arguments: block.input ?? {} });
  }
  return { content, toolCalls };
}

function safeParse(s: unknown): Record<string, unknown> {
  if (typeof s !== 'string') return (s as Record<string, unknown>) ?? {};
  try { return JSON.parse(s); } catch { return {}; }
}

// A thrown fetch (CORS / mixed-content / connection refused) never yields a
// Response, so translate it into something the operator can act on.
function networkError(e: unknown, cfg: LlmConfig): string {
  const local = PROVIDER_PRESETS[cfg.provider].local;
  const httpsPage = typeof window !== 'undefined' && window.location.protocol === 'https:';
  const httpTarget = /^http:\/\//i.test(cfg.baseUrl);
  const isLocalhost = /\/\/(localhost|127\.0\.0\.1)/i.test(cfg.baseUrl);
  if (httpsPage && httpTarget && !isLocalhost) {
    return `Blocked: the dashboard is HTTPS but the endpoint is http:// (${cfg.baseUrl}). Browsers block this ` +
      `"mixed content" — use an https endpoint, or open the dashboard over http.`;
  }
  if (local) {
    return `Couldn't reach ${PROVIDER_PRESETS[cfg.provider].label} at ${cfg.baseUrl}. Either the server isn't ` +
      `running / no model is loaded, or the browser is blocked by CORS. In LM Studio: Developer → Server ` +
      `settings → enable "CORS" (and "Serve on Local Network" if the dashboard isn't on this machine), then ` +
      `Start Server.`;
  }
  return `Couldn't reach ${cfg.baseUrl}. Check the endpoint and your connection (${(e as Error)?.message ?? 'network error'}).`;
}

async function friendlyError(res: Response, cfg: LlmConfig): Promise<string> {
  const txt = await res.text().catch(() => '');
  const detail = txt ? ` Server said: ${txt.slice(0, 200)}` : '';
  if (res.status === 404) {
    return PROVIDER_PRESETS[cfg.provider].local
      ? `${cfg.baseUrl} returned 404 — load a model in ${PROVIDER_PRESETS[cfg.provider].label} and make sure ` +
        `the Model name in Settings matches a loaded model.${detail}`
      : `Endpoint not found (404). Check the base URL/model in Settings.${detail}`;
  }
  if (res.status === 401 || res.status === 403) return 'Authentication failed — check the API key in Settings → Companion.';
  return `LLM error ${res.status}: ${txt.slice(0, 200)}`;
}
