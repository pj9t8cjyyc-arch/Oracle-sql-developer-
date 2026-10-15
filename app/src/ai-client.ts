import type { Api } from "./api";
import type { AiEvent, ChatRequest, ChatResult } from "./types";

export class AiAuthRequired extends Error {
  constructor(message: string) { super(message); this.name = "AiAuthRequired"; }
}

/**
 * Sends a chat and streams the answer. Events can arrive before the start call returns, so they are
 * kept per id. `cancel()` stops the answer; the promise then resolves with what arrived (`cancelled`).
 * Rejects with {@link AiAuthRequired} when the person must sign in again.
 */
export function chat(api: Api, request: ChatRequest, onDelta: (text: string) => void): { promise: Promise<ChatResult>; cancel(): void } {
  return chatWith(api, () => api.aiChatStart(request), onDelta);
}

/** Same as {@link chat} for any call that starts a streamed answer and returns its id. */
export function chatWith(api: Api, start: () => Promise<string>, onDelta: (text: string) => void): { promise: Promise<ChatResult>; cancel(): void } {
  let id: string | undefined;
  let cancelled = false;
  const buffered: AiEvent[] = [];
  let finish: (e: AiEvent) => void = () => {};
  const handle = (e: AiEvent) => {
    if (e.type === "delta") onDelta(e.text);
    else finish(e);
  };
  const promise = new Promise<ChatResult>((resolve, reject) => {
    let off: () => void = () => {};
    finish = (e) => {
      off();
      if (e.type === "done") resolve(e.result);
      else if (e.type === "error") reject(e.auth_required ? new AiAuthRequired(e.message) : new Error(e.message));
    };
    api.onAi((e) => {
      if (id === undefined) buffered.push(e);
      else if (e.id === id) handle(e);
    }).then((unlisten) => {
      off = unlisten;
      return start();
    }).then((started) => {
      id = started;
      if (cancelled) void api.aiCancel(started);
      for (const e of buffered.splice(0)) if (e.id === id) handle(e);
    }).catch((e) => { off(); reject(e instanceof Error ? e : new Error(String(e))); });
  });
  return { promise, cancel() { cancelled = true; if (id) void api.aiCancel(id); } };
}
