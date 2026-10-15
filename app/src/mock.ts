// Browser-only demo backend so the UI can be developed and screenshotted
// without Tauri or Oracle. The safe-mode check here is a crude stand-in for
// the Rust guard; it is NOT used in the real app.
import type { Api } from "./api";
import type { Analysis, ColumnInsight, Column, Completions, Hit, Row, RunResult, Suggestion, TableInsight, TableRef } from "./types";

const cols = (...n: string[]): Column[] => n.map((name) => ({ name, data_type: "VARCHAR2", nullable: true }));
const BIG = 250_000;

const data: Record<string, { columns: Column[]; total: number; row: (i: number) => Row }> = {
  employees: {
    columns: cols("EMPLOYEE_ID", "FIRST_NAME", "LAST_NAME", "SALARY", "DEPARTMENT_ID"),
    total: 107,
    row: (i) => [String(100 + i), ["Ann", "Raj", "Mei", "Luis"][i % 4], `Lastname${i}`, String(4000 + ((i * 137) % 9000)), i % 9 === 0 ? null : String(10 * (1 + (i % 11)))],
  },
  departments: {
    columns: cols("DEPARTMENT_ID", "DEPARTMENT_NAME"),
    total: 27,
    row: (i) => [String(10 * (i + 1)), `Dept ${i + 1}`],
  },
  big_table: {
    columns: cols("ID", "PAYLOAD", "CREATED"),
    total: BIG,
    row: (i) => [String(i + 1), `row-${i + 1}-${"x".repeat(i % 20)}`, `2026-01-${String((i % 28) + 1).padStart(2, "0")}`],
  },
};

function analyze(sql: string): Analysis {
  const s = sql.trim().replace(/;$/, "");
  const l = s.toLowerCase();
  const kind = l.startsWith("select") || l.startsWith("with") ? "select" : l.split(/\s+/)[0] || "empty";
  const warnings: Analysis["warnings"] = [];
  if ((kind === "update" || kind === "delete") && !/\bwhere\b/.test(l)) {
    warnings.push({ severity: "danger", message: "No WHERE clause: this will affect every row in the table." });
  }
  if (kind === "drop" || kind === "truncate") warnings.push({ severity: "danger", message: "DROP/TRUNCATE is permanent and cannot be rolled back." });
  return { kind, sql: s, warnings, requires_confirmation: warnings.some((w) => w.severity === "danger"), blocked: null, read_only: kind === "select" };
}


// ---------- Phase 1 demo data ----------
const ci = (name: string, data_type: string, o: Partial<ColumnInsight> = {}): ColumnInsight => ({
  name, data_type, nullable: true, comment: null, distinct: null, nulls: null, histogram: null,
  primary_key: false, unique: false, foreign_keys: [], checks: [], indexes: [], triggers: [], ...o,
});
const insights: Record<string, TableInsight> = {
  "HR.EMPLOYEES": {
    owner: "HR", table: "EMPLOYEES", comment: "Employee master data",
    columns: [
      ci("EMPLOYEE_ID", "NUMBER(6)", { nullable: false, primary_key: true, distinct: 107, nulls: 0, comment: "Primary key of the employee",
        indexes: [{ name: "EMP_EMP_ID_PK", position: 1, unique: true, leading: true, valid: true }],
        triggers: [{ name: "EMP_AUDIT_TRG", timing: "AFTER (row)", event: "INSERT OR UPDATE OR DELETE", enabled: true, reason: "any_update" }] }),
      ci("FIRST_NAME", "VARCHAR2(20)", { distinct: 91, nulls: 0, indexes: [{ name: "EMP_NAME_IX", position: 2, unique: false, leading: false, valid: true }] }),
      ci("LAST_NAME", "VARCHAR2(25)", { nullable: false, distinct: 102, nulls: 0, indexes: [{ name: "EMP_NAME_IX", position: 1, unique: false, leading: true, valid: true }] }),
      ci("SALARY", "NUMBER(8,2)", { distinct: 58, nulls: 0, checks: ["salary > 0"], comment: "Monthly salary in USD",
        triggers: [{ name: "SECURE_EMPLOYEES", timing: "BEFORE (row)", event: "UPDATE", enabled: true, reason: "update_of" }] }),
      ci("DEPARTMENT_ID", "NUMBER(4)", { distinct: 11, nulls: 1, histogram: "FREQUENCY",
        foreign_keys: [{ constraint: "EMP_DEPT_FK", ref_owner: "HR", ref_table: "DEPARTMENTS", ref_column: "DEPARTMENT_ID" }],
        indexes: [{ name: "EMP_DEPT_IX", position: 1, unique: false, leading: true, valid: true }],
        triggers: [{ name: "EMP_AUDIT_TRG", timing: "AFTER (row)", event: "INSERT OR UPDATE OR DELETE", enabled: true, reason: "referenced" }] }),
    ],
    indexes: [
      { name: "EMP_EMP_ID_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["EMPLOYEE_ID"] },
      { name: "EMP_NAME_IX", unique: false, valid: true, index_type: "NORMAL", columns: ["LAST_NAME", "FIRST_NAME"] },
      { name: "EMP_DEPT_IX", unique: false, valid: true, index_type: "NORMAL", columns: ["DEPARTMENT_ID"] },
    ],
    triggers: [
      { name: "EMP_AUDIT_TRG", timing: "AFTER (row)", event: "INSERT OR UPDATE OR DELETE", enabled: true, columns: ["DEPARTMENT_ID"] },
      { name: "SECURE_EMPLOYEES", timing: "BEFORE (row)", event: "UPDATE", enabled: true, columns: ["SALARY"] },
    ],
    referenced_by: [{ owner: "HR", table: "JOB_HISTORY", constraint: "JHIST_EMP_FK" }],
    uses: [{ owner: "HR", name: "DEPARTMENTS", object_type: "TABLE" }],
    used_by: [{ owner: "HR", name: "EMP_DETAILS_VIEW", object_type: "VIEW" }],
  },
  "HR.DEPARTMENTS": {
    owner: "HR", table: "DEPARTMENTS", comment: null,
    columns: [
      ci("DEPARTMENT_ID", "NUMBER(4)", { nullable: false, primary_key: true, distinct: 27, nulls: 0, indexes: [{ name: "DEPT_ID_PK", position: 1, unique: true, leading: true, valid: true }] }),
      ci("DEPARTMENT_NAME", "VARCHAR2(30)", { nullable: false, distinct: 27, nulls: 0 }),
    ],
    indexes: [{ name: "DEPT_ID_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["DEPARTMENT_ID"] }],
    triggers: [], referenced_by: [{ owner: "HR", table: "EMPLOYEES", constraint: "EMP_DEPT_FK" }], uses: [], used_by: [],
  },
};
const demoTables: Record<string, string[]> = { EMPLOYEES: insights["HR.EMPLOYEES"].columns.map((c) => c.name), DEPARTMENTS: ["DEPARTMENT_ID", "DEPARTMENT_NAME"], BIG_TABLE: ["ID", "PAYLOAD", "CREATED"] };

function mockRefs(sql: string): TableRef[] {
  const out: TableRef[] = [];
  const re = /\b(?:from|join)\s+(?:(\w+)\.)?(\w+)(?:\s+(?:as\s+)?(?!where|join|on|left|right|inner|group|order\b)(\w+))?/gi;
  for (const m of sql.matchAll(re)) out.push({ schema: m[1]?.toUpperCase() ?? null, name: m[2].toUpperCase(), alias: m[3]?.toUpperCase() ?? null });
  return out;
}

function mockComplete(sql: string, cursor: number): Completions {
  const before = sql.slice(0, cursor);
  const word = /[\w$#]*$/.exec(before)![0];
  const from = cursor - word.length;
  const refs = mockRefs(sql);
  const wl = word.toUpperCase();
  const hit = (l: string) => l.toUpperCase().includes(wl);
  const items: Suggestion[] = [];
  const dot = /(\w+)\.$/.exec(before.slice(0, from));
  if (dot) {
    const q = dot[1].toUpperCase();
    const r = refs.find((x) => x.alias === q || x.name === q);
    for (const c of demoTables[r?.name ?? q] ?? []) if (hit(c)) items.push({ label: c, insert: c, kind: "column", detail: demoTables[r?.name ?? q] ? "column" : null, score: 100 });
  } else if (/\b(from|join)\s+(\w*)$/i.test(before)) {
    for (const t of Object.keys(demoTables)) if (hit(t)) items.push({ label: t, insert: t, kind: "table", detail: `${demoTables[t].length} columns`, score: 50 });
  } else {
    for (const r of refs) for (const c of demoTables[r.name] ?? []) if (hit(c)) items.push({ label: c, insert: refs.length > 1 ? `${r.alias ?? r.name}.${c}` : c, kind: "column", detail: r.name, score: 100 });
  }
  return { from, items };
}

const searchCorpus: Hit[] = [
  { kind: "table", owner: "HR", name: "EMPLOYEES", parent: "", line: null, snippet: "Employee master data", score: -5 },
  { kind: "table", owner: "HR", name: "DEPARTMENTS", parent: "", line: null, snippet: "", score: -5 },
  { kind: "column", owner: "HR", name: "SALARY", parent: "EMPLOYEES", line: null, snippet: "Monthly salary in USD", score: -4 },
  { kind: "column", owner: "HR", name: "DEPARTMENT_ID", parent: "EMPLOYEES", line: null, snippet: "", score: -4 },
  { kind: "column", owner: "HR", name: "DEPARTMENT_ID", parent: "DEPARTMENTS", line: null, snippet: "", score: -4 },
  { kind: "source", owner: "HR", name: "SECURE_EMPLOYEES", parent: "TRIGGER", line: 7, snippet: "if :new.[salary] > 100000 then raise_application_error(...)", score: -2 },
  { kind: "table", owner: "SALES", name: "ORDERS", parent: "", line: null, snippet: "", score: -5 },
];

const cursors = new Map<number, { table: (typeof data)[string]; pos: number }>();
let nextHandle = 1;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const page = (t: (typeof data)[string], from: number, n: number): Row[] =>
  Array.from({ length: Math.max(0, Math.min(n, t.total - from)) }, (_, k) => t.row(from + k));

export const mockApi: Api = {
  listProfiles: async () => [{ name: "Demo (no database)", host: "localhost", port: 1521, service: "FREEPDB1", user: "hr", environment: "dev" }],
  saveProfile: async () => {},
  deleteProfile: async () => {},
  connect: async () => { await sleep(150); },
  disconnect: async () => {},
  async runQuery(sql, confirmed, pageSize) {
    await sleep(60);
    const analysis = analyze(sql);
    if (analysis.requires_confirmation && !confirmed) return { kind: "needs_confirmation", analysis } satisfies RunResult;
    if (analysis.kind !== "select") return { kind: "affected", count: 3, analysis };
    const name = Object.keys(data).find((k) => analysis.sql.toLowerCase().includes(k));
    if (!name) throw "ORA-00942: table or view does not exist";
    const t = data[name];
    const rows = page(t, 0, pageSize);
    const done = rows.length >= t.total;
    const handle = nextHandle++;
    if (!done) cursors.set(handle, { table: t, pos: rows.length });
    return { kind: "rows", handle, columns: t.columns, rows, done, analysis };
  },
  async fetchMore(handle, pageSize) {
    await sleep(40);
    const c = cursors.get(handle);
    if (!c) throw `unknown query handle ${handle}`;
    const rows = page(c.table, c.pos, pageSize);
    c.pos += rows.length;
    const done = c.pos >= c.table.total;
    if (done) cursors.delete(handle);
    return { rows, done, truncated: false };
  },
  closeQuery: async (h) => void cursors.delete(h),
  cancelQuery: async () => {},
  commit: async () => {},
  rollback: async () => {},
  listSchemas: async () => ["HR", "SALES"],
  listObjects: async (owner) =>
    owner === "HR"
      ? [
          { name: "BIG_TABLE", object_type: "TABLE", valid: true },
          { name: "DEPARTMENTS", object_type: "TABLE", valid: true },
          { name: "EMPLOYEES", object_type: "TABLE", valid: true },
          { name: "EMP_DETAILS_VIEW", object_type: "VIEW", valid: false },
          { name: "SECURE_EMPLOYEES", object_type: "TRIGGER", valid: true },
        ]
      : [{ name: "ORDERS", object_type: "TABLE", valid: true }],
  refreshMetadata: async () => { await sleep(80); return { tables: 3, columns: 10, indexed_docs: 13 }; },
  tableInsight: async (owner, table) => insights[`${owner}.${table}`] ?? { owner, table, comment: null, columns: (demoTables[table] ?? ["ID"]).map((n) => ci(n, "VARCHAR2(30)")), indexes: [], triggers: [], referenced_by: [], uses: [], used_by: [] },
  complete: async (sql, cursor) => mockComplete(sql, cursor),
  references: async (sql) => mockRefs(sql),
  searchObjects: async (text, owner, kinds, limit) => {
    const words = text.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
    if (!words.length) return [];
    const hay = (h: Hit) => `${h.name} ${h.parent} ${h.snippet}`.toLowerCase();
    return searchCorpus
      .filter((h) => words.every((w) => hay(h).split(/[^a-z0-9]+/).some((t) => t.startsWith(w))) && (!owner || h.owner === owner) && (!kinds.length || kinds.includes(h.kind)))
      .sort((a, b) => Number(b.name.toLowerCase() === text.toLowerCase()) - Number(a.name.toLowerCase() === text.toLowerCase()) || a.score - b.score)
      .slice(0, limit);
  },
  previewComment: async (owner, table, column, text) =>
    `COMMENT ON ${column ? `COLUMN "${owner}"."${table}"."${column}"` : `TABLE "${owner}"."${table}"`} IS '${text.replace(/'/g, "''")}'`,
  saveComment: async (owner, table, column, text) => {
    const t = insights[`${owner}.${table}`];
    if (t) { if (column) { const c = t.columns.find((x) => x.name === column); if (c) c.comment = text || null; } else t.comment = text || null; }
    return "ok";
  },
  listColumns: async (_o, table) => {
    const t = data[table.toLowerCase()];
    return (t?.columns ?? cols("ID")).map((c, i) => ({ name: c.name, data_type: i === 0 ? "NUMBER(6)" : "VARCHAR2(30 CHAR)", nullable: i !== 0 }));
  },
};
