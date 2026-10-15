// Demo-only stand-in for the Rust history store (SQLite + FTS5), persisted to
// localStorage so reloading the page behaves like restarting the app.
import type { HistoryHit, HistoryQuery, HistoryStatus, OutlineItem, Step, WorksheetFilter, WorksheetRec, WorksheetSave } from "./types";

interface MExec { hash: string; sql: string; conn_key: string; conn_name: string; owner: string; worksheet: string | null; at: number; ms: number; rows: number | null; status: "ok" | "error" | "cancelled"; error: string | null }
interface MStore { execs: MExec[]; pinned: string[]; ws: Record<string, WorksheetRec>; enabled: boolean }

const KEY = "ora-ide-mock-history-v1";
const DAY = 86_400_000;
const canon = (sql: string) => sql.replace(/--.*$/gm, "").replace(/\s+/g, " ").trim().replace(/;$/, "").toLowerCase();
const tokens = (s: string) => s.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
const kindOf = (sql: string) => { const w = /^\s*(?:--.*\n\s*)*(\w+)/.exec(sql)?.[1]?.toLowerCase() ?? "other"; return w === "with" ? "select" : ["select", "insert", "update", "delete", "merge"].includes(w) ? w : ["create", "alter", "drop", "truncate", "comment"].includes(w) ? "ddl" : w === "begin" || w === "declare" ? "plsql" : "other"; };
const tablesOf = (sql: string): string[] => [...new Set([...sql.matchAll(/\b(?:from|join|update|into)\s+([a-z0-9_$.]+)/gi)].map((m) => m[1].toUpperCase()).filter((t) => t !== "DUAL"))].sort();

function seed(): MStore {
  const hr = { key: "key-FREEPDB1", name: "HR · development", owner: "HR" };
  const sales = { key: "key-SALESPDB", name: "SALES · production", owner: "SALES" };
  const q: [typeof hr, string, number, ("ok" | "error")?][] = [
    [hr, "SELECT first_name, last_name, salary FROM hr.employees WHERE department_id = 50 ORDER BY salary DESC", 14],
    [hr, "SELECT department_id, COUNT(*) AS headcount, ROUND(AVG(salary)) AS avg_salary FROM hr.employees GROUP BY department_id ORDER BY headcount DESC", 6],
    [hr, "SELECT e.last_name, d.department_name FROM hr.employees e JOIN hr.departments d ON d.department_id = e.department_id WHERE d.department_name = 'Sales'", 9],
    [hr, "UPDATE hr.employees SET salary = salary * 1.05 WHERE department_id = 90", 1],
    [hr, "SELECT * FROM hr.job_history WHERE employee_id = 101", 3],
    [hr, "SELECT * FROM hr.employees WHERE hire_date > DATE '2020-01-01'", 2],
    [hr, "SELECT * FROM hr.employes", 1, "error"],
    [sales, "SELECT status, COUNT(*) AS orders, SUM(total) AS revenue FROM sales.orders GROUP BY status", 20],
    [sales, "SELECT * FROM sales.orders WHERE customer_id = 105 AND status = 'PAID'", 5],
    [sales, "SELECT c.name, SUM(o.total) AS spend FROM sales.customers c JOIN sales.orders o ON o.customer_id = c.customer_id GROUP BY c.name ORDER BY spend DESC FETCH FIRST 10 ROWS ONLY", 4],
    [sales, "SELECT country, COUNT(*) FROM sales.customers GROUP BY country", 2],
  ];
  const execs: MExec[] = [];
  const now = Date.now();
  q.forEach(([c, sql, runs, st], qi) => {
    for (let i = 0; i < runs; i++) {
      const age = Math.floor(((i + 1) / runs) * (190 - qi * 9) + ((qi * 7 + i * 13) % 6));
      execs.push({ hash: canon(sql), sql, conn_key: c.key, conn_name: c.name, owner: c.owner, worksheet: null, at: now - age * DAY - ((i * 977 + qi * 3331) % 80_000_000), ms: 20 + ((i * 37 + qi * 91) % 400), rows: st === "error" ? null : 5 + ((i * 17 + qi * 29) % 200), status: st ?? "ok", error: st === "error" ? 'ORA-00942: table or view "HR"."EMPLOYES" does not exist' : null });
    }
  });
  return { execs, pinned: [canon(q[7][1])], ws: {}, enabled: true };
}

function load(): MStore {
  try { const raw = localStorage.getItem(KEY); if (raw) return JSON.parse(raw) as MStore; } catch { /* private mode etc. */ }
  const s = seed();
  save(s);
  return s;
}
let store: MStore | undefined;
const st = () => (store ??= load());
function save(s: MStore) { try { localStorage.setItem(KEY, JSON.stringify(s)); } catch { /* ignore */ } }

export function record(e: Omit<MExec, "hash">) {
  if (!st().enabled) return;
  st().execs.push({ ...e, hash: canon(e.sql) });
  save(st());
}

function outline(sql: string): OutlineItem[] {
  const out: OutlineItem[] = [];
  let pos = 0;
  for (const part of sql.split(";")) {
    const text = part.trim();
    const start = sql.indexOf(part, pos);
    pos = start + part.length + 1;
    if (!text.replace(/--.*$/gm, "").trim()) continue;
    out.push({ kind: kindOf(text), tables: tablesOf(text).join(" "), preview: text.replace(/\s+/g, " ").slice(0, 140), start, end: start + part.length });
  }
  return out;
}
const autoTitle = (sql: string) => { const o = outline(sql); if (!o.length) return "Untitled"; const t = o[0].tables.split(" ").filter(Boolean).slice(0, 2).join(", "); return `${o[0].kind.toUpperCase()}${t ? " " + t : ""}${o.length > 1 ? ` (+${o.length - 1} more)` : ""}`; };

export const mockHistory = {
  async status(): Promise<HistoryStatus> {
    const s = st();
    return { enabled: s.enabled, persistent: true, stats: { executions: s.execs.length, distinct_queries: new Set(s.execs.map((e) => e.hash)).size, worksheets: Object.keys(s.ws).length, first_at: s.execs.length ? Math.min(...s.execs.map((e) => e.at)) : null, size_bytes: JSON.stringify(s).length } };
  },
  async setEnabled(enabled: boolean) { st().enabled = enabled; save(st()); },
  async search(q: HistoryQuery): Promise<HistoryHit[]> {
    const s = st(); const now = Date.now();
    const words = tokens(q.text ?? "");
    const by = new Map<string, MExec[]>();
    for (const e of s.execs) {
      if (q.conn_key && e.conn_key !== q.conn_key) continue;
      if (q.since && e.at < q.since) continue;
      if (q.until && e.at > q.until) continue;
      if (q.only_ok && e.status !== "ok") continue;
      (by.get(e.hash) ?? by.set(e.hash, []).get(e.hash)!).push(e);
    }
    const hits: HistoryHit[] = [];
    for (const [hash, list] of by) {
      list.sort((a, b) => a.at - b.at);
      const last = list[list.length - 1];
      const kind = kindOf(last.sql);
      if (q.kinds?.length && !q.kinds.includes(kind)) continue;
      const tables = tablesOf(last.sql);
      const hay = tokens(`${last.sql} ${tables.join(" ")}`);
      if (!words.every((w) => hay.some((t) => t.startsWith(w)))) continue;
      const pinned = s.pinned.includes(hash);
      if (q.pinned_only && !pinned) continue;
      const oks = list.filter((e) => e.status === "ok");
      const age = (now - last.at) / DAY;
      const score = words.length * 2 + 1.5 * Math.log(1 + list.length) + 2 * Math.pow(0.5, age / 30) + (pinned ? 5 : 0);
      let snippet: string | null = null;
      if (words.length) { snippet = last.sql.replace(/\s+/g, " "); for (const w of words) snippet = snippet.replace(new RegExp(`\\b(${w}[a-z0-9_]*)`, "i"), "[$1]"); }
      hits.push({ hash, sql: last.sql, kind, tables, runs: list.length, ok_runs: oks.length, first_run: list[0].at, last_run: last.at, avg_ms: oks.length ? oks.reduce((a, e) => a + e.ms, 0) / oks.length : null, last_status: last.status, last_conn_name: last.conn_name, last_conn_key: last.conn_key, last_owner: last.owner, pinned, snippet, score });
    }
    hits.sort((a, b) => (words.length ? b.score - a.score : Number(b.pinned) - Number(a.pinned) || b.last_run - a.last_run));
    return hits.slice(q.offset ?? 0, (q.offset ?? 0) + Math.min(q.limit ?? 50, 200));
  },
  async suggest(typed: string, connKey: string | null, limit: number): Promise<HistoryHit[]> {
    if (typed.trim().length < 3) return [];
    const norm = typed.replace(/\s+/g, " ").trim().toLowerCase();
    const hits = await this.search({ text: typed, conn_key: connKey, only_ok: true, limit: 50 });
    return hits.filter((h) => h.sql.replace(/\s+/g, " ").toLowerCase().startsWith(norm)).slice(0, limit);
  },
  async pin(hash: string, pinned: boolean) { const s = st(); s.pinned = s.pinned.filter((h) => h !== hash); if (pinned) s.pinned.push(hash); save(s); },
  async forget(hash: string) { const s = st(); s.execs = s.execs.filter((e) => e.hash !== hash); s.pinned = s.pinned.filter((h) => h !== hash); save(s); },
  async purge(before: number) { const s = st(); const n = s.execs.length; s.execs = s.execs.filter((e) => e.at >= before); save(s); return n - s.execs.length; },
  async wsSave(w: WorksheetSave): Promise<WorksheetRec | null> {
    const s = st(); const now = Date.now(); const old = s.ws[w.uuid];
    if (!old && !w.sql.trim()) return null;
    const rec: WorksheetRec = {
      uuid: w.uuid, conn_key: w.conn_key, conn_name: w.conn_name, owner: w.owner, sql: w.sql, outline: outline(w.sql),
      title: w.title?.trim() || (old?.custom_title ? old.title : autoTitle(w.sql)), custom_title: !!(w.title?.trim() || old?.custom_title),
      created_at: old?.created_at ?? now, updated_at: now, closed_at: null, pinned: old?.pinned ?? false, position: w.position, runs: 0,
    };
    s.ws[w.uuid] = rec; save(s);
    return { ...rec, runs: s.execs.filter((e) => e.worksheet === w.uuid).length };
  },
  async wsClose(uuid: string) { const s = st(); const w = s.ws[uuid]; if (w && w.closed_at === null) { w.closed_at = Date.now(); save(s); } },
  async wsPin(uuid: string, pinned: boolean) { const s = st(); if (s.ws[uuid]) { s.ws[uuid].pinned = pinned; save(s); } },
  async wsDelete(uuid: string) { const s = st(); delete s.ws[uuid]; save(s); },
  async wsList(f: WorksheetFilter): Promise<WorksheetRec[]> {
    const s = st(); const words = tokens(f.text ?? "");
    const list = Object.values(s.ws)
      .filter((w) => (f.state === "open" ? w.closed_at === null : f.state === "closed" ? w.closed_at !== null : true))
      .filter((w) => !f.conn_key || w.conn_key === f.conn_key)
      .filter((w) => { const hay = tokens(`${w.title} ${w.sql}`); return words.every((x) => hay.some((t) => t.startsWith(x))); })
      .map((w) => ({ ...w, runs: s.execs.filter((e) => e.worksheet === w.uuid).length }));
    list.sort(f.state === "open" ? (a, b) => a.position - b.position || a.created_at - b.created_at : (a, b) => Number(b.pinned) - Number(a.pinned) || b.updated_at - a.updated_at);
    return list.slice(0, f.limit ?? 100);
  },
  async wsGet(uuid: string) { const w = st().ws[uuid]; return w ? { ...w, runs: st().execs.filter((e) => e.worksheet === uuid).length } : null; },
  async wsSteps(uuid: string): Promise<Step[]> {
    return st().execs.filter((e) => e.worksheet === uuid).sort((a, b) => a.at - b.at).map((e) => ({ at: e.at, hash: e.hash, sql: e.sql, kind: kindOf(e.sql), status: e.status, duration_ms: e.ms, rows: e.rows, error: e.error }));
  },
};
