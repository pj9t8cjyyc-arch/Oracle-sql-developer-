// Demo-only stand-in for in-app updates: pretends version 0.2.0 is available.
import type { Api } from "./api";
import type { UpdateProgress, UpdateStatus } from "./types";

let st: UpdateStatus = { enabled: true, current_version: "0.1.0", endpoint: "https://github.com/pj9t8cjyyc-arch/Oracle-sql-developer-/releases/latest/download/latest.json", custom_endpoint: false, auto_check: true };
const listeners = new Set<(p: UpdateProgress) => void>();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const BUILT_IN = st.endpoint;

export function updateApi(): Pick<Api, "updateStatus" | "updateSet" | "updateCheck" | "updateInstall" | "onUpdateProgress"> {
  return {
    updateStatus: async () => st,
    updateSet: async (auto, endpoint) => {
      const e = (endpoint ?? "").trim();
      if (e && !/^https:\/\/[^\s@/]+(\/\S*)?$/.test(e) && !/^http:\/\/(localhost|127\.0\.0\.1)/.test(e)) throw "the update address must use https:// (http:// is allowed for localhost only)";
      st = { ...st, auto_check: auto, custom_endpoint: !!e, endpoint: e || BUILT_IN };
      return st;
    },
    updateCheck: async () => { await sleep(300); return { version: "0.2.0", date: "2026-10-20", notes: "Sandbox clone, PL/SQL output and compile errors, grey-text completion." }; },
    updateInstall: async () => {
      for (let i = 1; i <= 10; i++) { await sleep(120); listeners.forEach((l) => l({ downloaded: i * 2_000_000, total: 20_000_000 })); }
      throw "Demo: the real app would now restart.";
    },
    onUpdateProgress: async (cb) => { listeners.add(cb); return () => void listeners.delete(cb); },
  };
}
