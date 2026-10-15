// Browser-only demo backend so the UI can be developed and screenshotted
// without Tauri or Oracle. The safe-mode check here is a crude stand-in for
// the Rust guard; it is NOT used in the real app.
import type { Api } from "./api";
import type { Analysis, Column, Row, RunResult } from "./types";

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
  listColumns: async (_o, table) => {
    const t = data[table.toLowerCase()];
    return (t?.columns ?? cols("ID")).map((c, i) => ({ name: c.name, data_type: i === 0 ? "NUMBER(6)" : "VARCHAR2(30 CHAR)", nullable: i !== 0 }));
  },
};
