import type { ContextPolicy } from "./types";

const POLICY_KEY = "ora.ask.policy";
const ON_KEY = "ora.assist.on";

export function loadPolicy(): ContextPolicy {
  const d: ContextPolicy = { include_comments: true, include_history: true, exclude: [], max_tokens: 6000, max_objects: 25 };
  try { return { ...d, ...JSON.parse(localStorage.getItem(POLICY_KEY) ?? "{}") }; } catch { return d; }
}
export function savePolicy(p: ContextPolicy) { try { localStorage.setItem(POLICY_KEY, JSON.stringify(p)); } catch { /* optional */ } }

/** Automatic help is on unless the person turned it off. */
export function loadAssistOn(): boolean { try { return localStorage.getItem(ON_KEY) !== "0"; } catch { return true; } }
export function saveAssistOn(on: boolean) { try { localStorage.setItem(ON_KEY, on ? "1" : "0"); } catch { /* optional */ } }
