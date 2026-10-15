// Demo-only stand-in for the AI provider layer (ora-core `ai`). Streams a canned reply.
import type { Api } from "./api";
import type { AiConfig, AiEvent, AiStatus } from "./types";

let cfg: AiConfig = { provider: "mock", base_url: "", path: "/chat/completions", model: "demo-model", auth_header: "Authorization", auth_scheme: "Bearer", extra_headers: {}, timeout_secs: 120, stream: true, max_tokens: null, temperature: null };
let token: { expires: number | null } | null = null;
const listeners = new Set<(e: AiEvent) => void>();
const cancelled = new Set<string>();
let seq = 0;

function status(): AiStatus {
  let err: string | null = null;
  if (cfg.provider !== "mock") {
    if (!/^https:\/\//.test(cfg.base_url) && !/^http:\/\/(localhost|127\.0\.0\.1)/.test(cfg.base_url)) err = "the endpoint must start with https:// (http is allowed for localhost only)";
    else if (!cfg.model.trim()) err = "choose a model name";
  }
  const valid = token !== null && (token.expires === null || token.expires - 30_000 > Date.now());
  return { configured: err === null, signed_in: valid, expires_at_ms: token?.expires ?? null, provider: cfg.provider, model: cfg.model, base_url: cfg.base_url, config_error: err };
}

export function aiApi(): Pick<Api, "aiStatus" | "aiGetConfig" | "aiSaveConfig" | "aiSetToken" | "aiClearToken" | "aiTest" | "aiChatStart" | "aiCancel" | "onAi"> {
  return {
    aiStatus: async () => status(),
    aiGetConfig: async () => cfg,
    aiSaveConfig: async (c) => { const prev = cfg; cfg = c; const s = status(); if (!s.configured) { cfg = prev; throw s.config_error!; } return s; },
    aiSetToken: async (t, e) => { if (!t.trim()) throw "the token is empty or not valid"; token = { expires: e }; return status(); },
    aiClearToken: async () => { token = null; return status(); },
    aiTest: async () => {
      const s = status();
      if (!s.configured) return { ok: false, text: "", elapsed_ms: 0, error: s.config_error, auth_required: false };
      if (cfg.provider !== "mock" && !s.signed_in) return { ok: false, text: "", elapsed_ms: 0, error: "AI sign-in required: you are not signed in to the AI service", auth_required: true };
      await new Promise((r) => setTimeout(r, 150));
      return { ok: true, text: "OK (demo)", elapsed_ms: 150, error: null, auth_required: false };
    },
    aiChatStart: async (req) => {
      const id = `ai${++seq}`;
      const s = status();
      const emit = (e: AiEvent) => listeners.forEach((l) => l(e));
      setTimeout(async () => {
        if (cfg.provider !== "mock" && !s.signed_in) { emit({ type: "error", id, message: "AI sign-in required: you are not signed in to the AI service", auth_required: true }); return; }
        const last = [...req.messages].reverse().find((m) => m.role === "user")?.content ?? "";
        let text = "";
        for (const w of `mock reply to: ${last}`.split(" ")) {
          if (cancelled.has(id)) { emit({ type: "done", id, result: { text, usage: null, finish_reason: null, cancelled: true, elapsed_ms: 0 } }); return; }
          const piece = text ? ` ${w}` : w;
          text += piece; emit({ type: "delta", id, text: piece });
          await new Promise((r) => setTimeout(r, 40));
        }
        emit({ type: "done", id, result: { text, usage: { prompt_tokens: 10, completion_tokens: 6 }, finish_reason: "stop", cancelled: false, elapsed_ms: 300 } });
      }, 20);
      return id;
    },
    aiCancel: async (id) => void cancelled.add(id),
    onAi: async (cb) => { listeners.add(cb); return () => void listeners.delete(cb); },
  };
}
