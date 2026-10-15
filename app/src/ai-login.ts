import type { Api } from "./api";

/**
 * The integration point for the enterprise sign-in.
 *
 * Call this from your existing login code as soon as it has a token:
 *
 *     import { provideAiToken } from "./ai-login";
 *     provideAiToken(api, token, expiresAtMs);   // expiresAtMs: Unix time in milliseconds, if known
 *
 * The token goes to the app's core and is kept in memory only. It is never written to disk and
 * never returned to the page. Call `api.aiClearToken()` when the person signs out.
 */
export async function provideAiToken(api: Api, token: string, expiresAtMs?: number | null) {
  return api.aiSetToken(token, expiresAtMs ?? null);
}

/** For login code that is not a module: `window.oraIdeProvideAiToken(token, expiresAtMs)`. */
export function exposeAiLogin(api: Api) {
  (window as unknown as { oraIdeProvideAiToken?: (t: string, e?: number) => Promise<unknown> }).oraIdeProvideAiToken = (t, e) => provideAiToken(api, t, e);
}
