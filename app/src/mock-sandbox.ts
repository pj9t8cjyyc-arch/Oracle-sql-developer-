// Demo-only stand-in for the sandbox (ora-core `sandbox`). The real one is a SQLite file; this keeps
// copies in memory and understands only simple statements, enough to show the screens.
import type { Api } from "./api";
import type { ClonePlan, CloneSpec, SandboxInfo, SandboxRun, SbTableMeta } from "./types";
import { runJob } from "./mock-transfer";

export interface SandboxDeps {
  tables(conn: string, owner: string): { name: string; rows: number; columns: { name: string; type: string; nullable: boolean }[]; parents: string[]; pk: string[] }[];
  data(conn: string, table: string): { columns: string[]; rows: (string | null)[][] } | undefined;
  connName(conn: string): string;
}

interface Box { info: SandboxInfo; data: Tables; base?: Tables }
const boxes = new Map<string, Box>();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
type Tables = Map<string, { columns: string[]; rows: (string | null)[][] }>;
const cloneTables = (m: Tables): Tables => new Map([...m].map(([k, v]) => [k, { columns: [...v.columns], rows: v.rows.map((r) => [...r]) }]));

export function sandboxApi(deps: SandboxDeps): Pick<Api, "sandboxTables" | "sandboxPlan" | "sandboxCloneStart" | "sandboxList" | "sandboxDelete" | "sandboxReset" | "sandboxRun"> {
  const chosen = (conn: string, owner: string, spec: CloneSpec) => {
    const all = deps.tables(conn, owner);
    let names = spec.all ? all.map((t) => t.name) : spec.tables.map((t) => t.toUpperCase());
    for (const n of names) if (!all.some((t) => t.name === n)) throw `table ${n} was not found in this schema (or is not a plain table)`;
    if (names.length === 0) throw "choose at least one table, or the whole schema";
    const parents: string[] = [];
    if (spec.include_fk_parents) for (let i = 0; i < names.length + parents.length; i++) for (const p of all.find((t) => t.name === [...names, ...parents][i])?.parents ?? []) if (![...names, ...parents].includes(p) && all.some((t) => t.name === p)) parents.push(p);
    names = [...names, ...parents];
    return { all, names, parents };
  };
  return {
    sandboxTables: async (c, o) => deps.tables(c, o).map((t) => ({ name: t.name, rows: t.rows })),
    sandboxPlan: async (c, o, spec) => {
      const { all, names, parents } = chosen(c, o, spec);
      const tables = names.map((n) => { const r = all.find((t) => t.name === n)!.rows; return { name: n, source_rows: r, rows_to_copy: spec.rows_per_table ? Math.min(r, spec.rows_per_table) : r, added_as_parent: parents.includes(n) }; });
      const plan: ClonePlan = { tables, rows_to_copy: tables.reduce((s, t) => s + (t.rows_to_copy ?? 0), 0), estimated: true };
      return plan;
    },
    sandboxCloneStart: async (c, o, name, spec) => {
      if (!/^[A-Za-z0-9_][A-Za-z0-9_-]{0,59}$/.test(name)) throw "a sandbox name is 1-60 letters, digits, - or _";
      if (boxes.has(name)) throw `a sandbox named ${name} already exists`;
      const { all, names, parents } = chosen(c, o, spec);
      const total = names.reduce((s, n) => s + all.find((t) => t.name === n)!.rows, 0);
      return runJob("sandbox", total, () => {
        const data = new Map<string, { columns: string[]; rows: (string | null)[][] }>();
        const tables: SbTableMeta[] = names.map((n) => {
          const t = all.find((x) => x.name === n)!;
          const d = deps.data(c, n) ?? { columns: t.columns.map((x) => x.name), rows: [] };
          const rows = d.rows.slice(0, spec.rows_per_table ?? d.rows.length).map((r) => r.map((v, i) => (spec.mask.some((m) => m.toUpperCase() === d.columns[i].toUpperCase() || m.toUpperCase() === `${n}.${d.columns[i]}`.toUpperCase()) ? null : v)));
          data.set(n, { columns: d.columns, rows });
          return { name: n, source_rows: t.rows, rows: rows.length, truncated: rows.length < t.rows, parents: t.parents, primary_key: t.pk, added_as_parent: parents.includes(n),
            columns: t.columns.map((x) => ({ name: x.name, oracle_type: x.type, sqlite_type: /NUMBER/.test(x.type) ? "NUMERIC" : "TEXT", masked: spec.mask.some((m) => m.toUpperCase() === x.name || m.toUpperCase() === `${n}.${x.name}`), skipped: false })) };
        });
        const meta = { name, source: deps.connName(c), owner: o, taken_at: Date.now(), spec, tables };
        boxes.set(name, { info: { meta, bytes: tables.reduce((s, t) => s + t.rows * 120, 8192), modified: false }, data });
        return meta;
      });
    },
    sandboxList: async () => [...boxes.values()].map((b) => b.info).sort((a, b) => b.meta.taken_at - a.meta.taken_at),
    sandboxDelete: async (name) => { if (!boxes.delete(name)) throw `no sandbox named ${name}`; },
    sandboxReset: async (name) => {
      const b = boxes.get(name); if (!b) throw `no sandbox named ${name}`;
      if (!b.base) return false;
      b.data = cloneTables(b.base); b.base = undefined; b.info.modified = false;
      return true;
    },
    sandboxRun: async (name, sql): Promise<SandboxRun> => {
      await sleep(40);
      const b = boxes.get(name); if (!b) throw `no sandbox named ${name}`;
      const t0 = performance.now();
      let s = sql.trim().replace(/;$/, "");
      const changes: string[] = [];
      if (/^\s*(begin|declare|exec|call)\b/i.test(s)) throw "PL/SQL cannot run in the sandbox (SQLite has no PL/SQL). Run it on the real database.";
      if (/\brownum\b/i.test(s)) throw "ROWNUM does not exist in SQLite. Use LIMIT n, or ROW_NUMBER() OVER (...).";
      const noDual = s.replace(/\s+from\s+dual\b/i, ""); if (noDual !== s) { changes.push("removed FROM DUAL"); s = noDual; }
      const fetch = /\s+fetch\s+first\s+(\d+)\s+rows?\s+only$/i.exec(s); if (fetch) { s = s.replace(fetch[0], ` LIMIT ${fetch[1]}`); changes.push("FETCH FIRST n ROWS ONLY -> LIMIT n"); }
      const done = (r: Partial<SandboxRun>): SandboxRun => ({ columns: [], rows: [], affected: null, truncated: false, elapsed_ms: Math.round(performance.now() - t0), translated: s !== sql.trim().replace(/;$/, "") ? s : null, changes, wrote: false, ...r });
      let m = /^select\s+count\(\*\)\s+from\s+(\w+)$/i.exec(s);
      if (m) { const d = b.data.get(m[1].toUpperCase()); if (!d) throw `no such table: ${m[1]} (only the cloned tables exist here; clone it, or tick "include tables they point to")`; return done({ columns: ["COUNT(*)"], rows: [[String(d.rows.length)]] }); }
      m = /^select\s+\*\s+from\s+(\w+)(?:\s+limit\s+(\d+))?$/i.exec(s);
      if (m) { const d = b.data.get(m[1].toUpperCase()); if (!d) throw `no such table: ${m[1]} (only the cloned tables exist here; clone it, or tick "include tables they point to")`; return done({ columns: d.columns, rows: d.rows.slice(0, m[2] ? Number(m[2]) : 1000) }); }
      m = /^(delete\s+from|update)\s+(\w+)/i.exec(s);
      if (m) {
        const d = b.data.get(m[2].toUpperCase()); if (!d) throw `no such table: ${m[2]}`;
        if (!b.base) b.base = cloneTables(b.data);
        const n = /where/i.test(s) ? Math.min(1, d.rows.length) : d.rows.length;
        if (/^delete/i.test(s)) d.rows.splice(0, n);
        b.info.modified = true;
        return done({ affected: n, wrote: true });
      }
      throw "The demo sandbox understands only SELECT * / COUNT(*), UPDATE and DELETE. The real sandbox runs any SQLite statement.";
    },
  };
}
