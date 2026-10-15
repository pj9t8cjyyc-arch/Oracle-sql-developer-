/** "3 days ago", "7 months ago": coarse on purpose, history is browsed, not audited. */
export function timeAgo(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 45) return "just now";
  const units: [number, string][] = [[60, "minute"], [3600, "hour"], [86400, "day"], [86400 * 30, "month"], [86400 * 365, "year"]];
  let out = "just now";
  for (let i = 0; i < units.length; i++) {
    const [sec, name] = units[i];
    if (s >= sec) { const n = Math.floor(s / sec); out = `${n} ${name}${n === 1 ? "" : "s"} ago`; }
  }
  return out;
}

export const fmtMs = (ms: number | null): string => (ms === null ? "—" : ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`);
export const fmtDateTime = (ms: number): string => new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
export const fmtBytes = (n: number): string => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`);
