// Browser-only demo backend: two fake databases so the UI (multi-connection,
// dictionary browser, insight) can be developed and screenshotted without
// Tauri or Oracle. The safe-mode check is a crude stand-in for the Rust guard;
// none of this runs in the real app.
import type { Api } from "./api";
import { mockHistory, record } from "./mock-history";
import { graphApi } from "./mock-graph";
import { transferApi } from "./mock-transfer";
import type {
  Analysis, Column, ColumnInsight, Completions, ConnInfo, Hit, ObjectDeps, ObjectEntry, ObjectKind, Profile, Row, RunResult, SeqUse,
  SequenceInfo, Suggestion, TableInsight, TableRef, TriggerInfo,
} from "./types";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const cols = (...n: string[]): Column[] => n.map((name) => ({ name, data_type: "VARCHAR2", nullable: true }));

const ci = (name: string, data_type: string, o: Partial<ColumnInsight> = {}): ColumnInsight => ({
  name, data_type, nullable: true, comment: null, distinct: null, nulls: null, histogram: null,
  primary_key: false, unique: false, foreign_keys: [], checks: [], indexes: [], triggers: [], sequences: [], ...o,
});
const seq = (owner: string, name: string, via: SeqUse["via"], via_name: string, columns: string[], o: Partial<SeqUse> = {}): SeqUse => ({
  owner, name, via, via_name, generation: null, min: "1", max: "9999999999999999999999999999", increment: "1", cycle: false, cache: "20", last_number: "1001", columns, ...o,
});
const trg = (owner: string, name: string, timing: string, event: string, o: Partial<TriggerInfo> = {}): TriggerInfo => ({
  name, owner, timing, event, enabled: true, columns: [], when_clause: null, sequences: [], ...o,
});
const entry = (name: string, o: Partial<ObjectEntry> = {}): ObjectEntry => ({ name, status: "VALID", ok: true, detail: null, parent: null, ...o });

interface Data { columns: Column[]; total: number; row: (i: number) => Row }
interface Db {
  user: string;
  schemas: string[];
  objects: Record<string, Partial<Record<ObjectKind, ObjectEntry[]>>>;
  insights: Record<string, TableInsight>;
  sources: Record<string, string>;
  seqs: Record<string, SequenceInfo>;
  data: Record<string, Data>;
}

const seqInfo = (owner: string, name: string, o: Partial<SequenceInfo> = {}): SequenceInfo => ({
  owner, name, min: "1", max: "9999999999999999999999999999", increment: "1", cycle: false, ordered: false, cache: "20", last_number: "1001", ...o,
});

// ------------------------------------------------------------------ HR (dev)
const hrEmp: TableInsight = {
  owner: "HR", table: "EMPLOYEES", comment: "Employee master data",
  columns: [
    ci("EMPLOYEE_ID", "NUMBER(6)", {
      nullable: false, primary_key: true, distinct: 107, nulls: 0, comment: "Primary key of the employee",
      indexes: [{ name: "EMP_EMP_ID_PK", position: 1, unique: true, leading: true, valid: true }],
      triggers: [
        { name: "EMP_BI_TRG", timing: "BEFORE (row)", event: "INSERT", enabled: true, reason: "referenced" },
        { name: "EMP_AUDIT_TRG", timing: "AFTER (row)", event: "INSERT OR UPDATE OR DELETE", enabled: true, reason: "any_update" },
      ],
      sequences: [{ owner: "HR", name: "EMPLOYEES_SEQ", via: "trigger", via_name: "EMP_BI_TRG" }],
    }),
    ci("FIRST_NAME", "VARCHAR2(20)", { distinct: 91, nulls: 0, indexes: [{ name: "EMP_NAME_IX", position: 2, unique: false, leading: false, valid: true }] }),
    ci("LAST_NAME", "VARCHAR2(25)", { nullable: false, distinct: 102, nulls: 0, indexes: [{ name: "EMP_NAME_IX", position: 1, unique: false, leading: true, valid: true }] }),
    ci("EMAIL", "VARCHAR2(25)", { nullable: false, unique: true, distinct: 107, nulls: 0, indexes: [{ name: "EMP_EMAIL_UK", position: 1, unique: true, leading: true, valid: true }] }),
    ci("SALARY", "NUMBER(8,2)", {
      distinct: 58, nulls: 0, checks: ["salary > 0"], comment: "Monthly salary in USD",
      triggers: [{ name: "SECURE_EMPLOYEES", timing: "BEFORE (row)", event: "UPDATE", enabled: true, reason: "update_of" }],
    }),
    ci("DEPARTMENT_ID", "NUMBER(4)", {
      distinct: 11, nulls: 1, histogram: "FREQUENCY",
      foreign_keys: [{ constraint: "EMP_DEPT_FK", ref_owner: "HR", ref_table: "DEPARTMENTS", ref_column: "DEPARTMENT_ID" }],
      indexes: [{ name: "EMP_DEPT_IX", position: 1, unique: false, leading: true, valid: true }],
      triggers: [{ name: "EMP_AUDIT_TRG", timing: "AFTER (row)", event: "INSERT OR UPDATE OR DELETE", enabled: true, reason: "referenced" }],
    }),
  ],
  indexes: [
    { name: "EMP_EMP_ID_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["EMPLOYEE_ID"] },
    { name: "EMP_EMAIL_UK", unique: true, valid: true, index_type: "NORMAL", columns: ["EMAIL"] },
    { name: "EMP_NAME_IX", unique: false, valid: true, index_type: "NORMAL", columns: ["LAST_NAME", "FIRST_NAME"] },
    { name: "EMP_DEPT_IX", unique: false, valid: true, index_type: "NORMAL", columns: ["DEPARTMENT_ID"] },
  ],
  triggers: [
    trg("HR", "EMP_BI_TRG", "BEFORE (row)", "INSERT", { columns: ["EMPLOYEE_ID"], when_clause: "new.employee_id IS NULL", sequences: ["EMPLOYEES_SEQ"] }),
    trg("HR", "EMP_AUDIT_TRG", "AFTER (row)", "INSERT OR UPDATE OR DELETE", { columns: ["DEPARTMENT_ID", "EMPLOYEE_ID"] }),
    trg("HR", "SECURE_EMPLOYEES", "BEFORE (row)", "UPDATE", { columns: ["SALARY"] }),
  ],
  sequences: [
    seq("HR", "EMPLOYEES_SEQ", "trigger", "EMP_BI_TRG", ["EMPLOYEE_ID"], { last_number: "207", cache: "20" }),
    seq("HR", "AUDIT_SEQ", "default", "EMPLOYEES", [], { last_number: "5400", cache: "100" }),
  ],
  referenced_by: [{ owner: "HR", table: "JOB_HISTORY", constraint: "JHIST_EMP_FK" }],
  uses: [{ owner: "HR", name: "DEPARTMENTS", object_type: "TABLE" }],
  used_by: [{ owner: "HR", name: "EMP_DETAILS_VIEW", object_type: "VIEW" }],
};
const hrDept: TableInsight = {
  owner: "HR", table: "DEPARTMENTS", comment: null,
  columns: [
    ci("DEPARTMENT_ID", "NUMBER(4)", {
      nullable: false, primary_key: true, distinct: 27, nulls: 0,
      indexes: [{ name: "DEPT_ID_PK", position: 1, unique: true, leading: true, valid: true }],
      sequences: [{ owner: "HR", name: "DEPARTMENTS_SEQ", via: "trigger", via_name: "DEPT_BI_TRG" }],
      triggers: [{ name: "DEPT_BI_TRG", timing: "BEFORE (row)", event: "INSERT", enabled: true, reason: "referenced" }],
    }),
    ci("DEPARTMENT_NAME", "VARCHAR2(30)", { nullable: false, distinct: 27, nulls: 0 }),
  ],
  indexes: [{ name: "DEPT_ID_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["DEPARTMENT_ID"] }],
  triggers: [
    trg("HR", "DEPT_BI_TRG", "BEFORE (row)", "INSERT", { columns: ["DEPARTMENT_ID"], sequences: ["DEPARTMENTS_SEQ"] }),
    trg("HR", "DEPT_LOG_TRG", "AFTER (statement)", "DELETE", { enabled: false }),
  ],
  sequences: [seq("HR", "DEPARTMENTS_SEQ", "trigger", "DEPT_BI_TRG", ["DEPARTMENT_ID"], { increment: "10", last_number: "280", cache: "NOCACHE" })],
  referenced_by: [{ owner: "HR", table: "EMPLOYEES", constraint: "EMP_DEPT_FK" }],
  uses: [], used_by: [],
};
const hrJob: TableInsight = {
  owner: "HR", table: "JOB_HISTORY", comment: "Past jobs of employees",
  columns: [
    ci("EMPLOYEE_ID", "NUMBER(6)", {
      nullable: false, primary_key: true, distinct: 10, nulls: 0,
      foreign_keys: [{ constraint: "JHIST_EMP_FK", ref_owner: "HR", ref_table: "EMPLOYEES", ref_column: "EMPLOYEE_ID" }],
      indexes: [{ name: "JHIST_EMP_IX", position: 1, unique: false, leading: true, valid: false }],
    }),
    ci("START_DATE", "DATE", { nullable: false }), ci("END_DATE", "DATE", { nullable: false }),
  ],
  indexes: [{ name: "JHIST_EMP_IX", unique: false, valid: false, index_type: "NORMAL", columns: ["EMPLOYEE_ID"] }],
  triggers: [], sequences: [], referenced_by: [], uses: [{ owner: "HR", name: "EMPLOYEES", object_type: "TABLE" }], used_by: [],
};

const hr: Db = {
  user: "HR",
  schemas: ["HR", "SCOTT"],
  objects: {
    HR: {
      TABLE: [entry("BIG_TABLE"), entry("DEPARTMENTS"), entry("EMPLOYEES"), entry("JOB_HISTORY")],
      VIEW: [entry("EMP_DETAILS_VIEW", { status: "INVALID", ok: false })],
      INDEX: [
        entry("DEPT_ID_PK", { parent: "DEPARTMENTS", detail: "UNIQUE · NORMAL · on DEPARTMENTS" }),
        entry("EMP_DEPT_IX", { parent: "EMPLOYEES", detail: "NONUNIQUE · NORMAL · on EMPLOYEES" }),
        entry("EMP_EMAIL_UK", { parent: "EMPLOYEES", detail: "UNIQUE · NORMAL · on EMPLOYEES" }),
        entry("EMP_EMP_ID_PK", { parent: "EMPLOYEES", detail: "UNIQUE · NORMAL · on EMPLOYEES" }),
        entry("EMP_NAME_IX", { parent: "EMPLOYEES", detail: "NONUNIQUE · NORMAL · on EMPLOYEES" }),
        entry("JHIST_EMP_IX", { parent: "JOB_HISTORY", status: "UNUSABLE", ok: false, detail: "NONUNIQUE · NORMAL · on JOB_HISTORY" }),
      ],
      SEQUENCE: [
        entry("AUDIT_SEQ", { detail: "+1 · last 5400" }), entry("DEPARTMENTS_SEQ", { detail: "+10 · last 280" }), entry("EMPLOYEES_SEQ", { detail: "+1 · last 207" }),
      ],
      TRIGGER: [
        entry("DEPT_BI_TRG", { parent: "DEPARTMENTS", detail: "BEFORE EACH ROW · INSERT · on DEPARTMENTS" }),
        entry("DEPT_LOG_TRG", { parent: "DEPARTMENTS", status: "DISABLED", ok: false, detail: "AFTER STATEMENT · DELETE · on DEPARTMENTS" }),
        entry("EMP_AUDIT_TRG", { parent: "EMPLOYEES", detail: "AFTER EACH ROW · INSERT OR UPDATE OR DELETE · on EMPLOYEES" }),
        entry("EMP_BI_TRG", { parent: "EMPLOYEES", detail: "BEFORE EACH ROW · INSERT · on EMPLOYEES" }),
        entry("SECURE_EMPLOYEES", { parent: "EMPLOYEES", detail: "BEFORE EACH ROW · UPDATE · on EMPLOYEES" }),
      ],
      PROCEDURE: [entry("ADD_JOB_HISTORY")],
      FUNCTION: [entry("GET_SALARY")],
      PACKAGE: [entry("EMP_PKG")],
      TYPE: [entry("ADDRESS_T")],
      SYNONYM: [entry("EMP", { detail: "→ HR.EMPLOYEES" })],
    },
    SCOTT: { TABLE: [entry("DEPT"), entry("EMP")], SEQUENCE: [entry("EMP_SEQ", { detail: "+1 · last 8000" })] },
  },
  insights: { "HR.EMPLOYEES": hrEmp, "HR.DEPARTMENTS": hrDept, "HR.JOB_HISTORY": hrJob },
  sources: {
    "HR.EMP_BI_TRG.TRIGGER": "TRIGGER emp_bi_trg\nBEFORE INSERT ON employees\nFOR EACH ROW\nWHEN (new.employee_id IS NULL)\nBEGIN\n  :new.employee_id := employees_seq.NEXTVAL;\nEND;",
    "HR.SECURE_EMPLOYEES.TRIGGER": "TRIGGER secure_employees\nBEFORE UPDATE OF salary ON employees\nFOR EACH ROW\nBEGIN\n  IF :new.salary > 100000 THEN\n    raise_application_error(-20001, 'Salary above policy limit');\n  END IF;\nEND;",
    "HR.EMP_AUDIT_TRG.TRIGGER": "TRIGGER emp_audit_trg\nAFTER INSERT OR UPDATE OR DELETE ON employees\nFOR EACH ROW\nBEGIN\n  INSERT INTO emp_audit(id, dept, at) VALUES (audit_seq.NEXTVAL, :new.department_id, SYSDATE);\nEND;",
    "HR.ADD_JOB_HISTORY.PROCEDURE": "PROCEDURE add_job_history(p_emp_id NUMBER, p_start DATE, p_end DATE) IS\nBEGIN\n  INSERT INTO job_history(employee_id, start_date, end_date) VALUES (p_emp_id, p_start, p_end);\nEND add_job_history;",
    "HR.GET_SALARY.FUNCTION": "FUNCTION get_salary(p_emp_id NUMBER) RETURN NUMBER IS\n  v NUMBER;\nBEGIN\n  SELECT salary INTO v FROM employees WHERE employee_id = p_emp_id;\n  RETURN v;\nEND get_salary;",
    "HR.EMP_PKG.PACKAGE": "PACKAGE emp_pkg IS\n  PROCEDURE raise_salary(p_id NUMBER, p_pct NUMBER);\n  FUNCTION headcount(p_dept NUMBER) RETURN NUMBER;\nEND emp_pkg;",
    "HR.EMP_PKG.PACKAGE BODY": "PACKAGE BODY emp_pkg IS\n  PROCEDURE raise_salary(p_id NUMBER, p_pct NUMBER) IS\n  BEGIN\n    UPDATE employees SET salary = salary * (1 + p_pct / 100) WHERE employee_id = p_id;\n  END;\n  FUNCTION headcount(p_dept NUMBER) RETURN NUMBER IS\n    n NUMBER;\n  BEGIN\n    SELECT COUNT(*) INTO n FROM employees WHERE department_id = p_dept;\n    RETURN n;\n  END;\nEND emp_pkg;",
    "HR.ADDRESS_T.TYPE": "TYPE address_t AS OBJECT (street VARCHAR2(60), city VARCHAR2(40), zip VARCHAR2(10));",
  },
  seqs: {
    "HR.EMPLOYEES_SEQ": seqInfo("HR", "EMPLOYEES_SEQ", { last_number: "207" }),
    "HR.DEPARTMENTS_SEQ": seqInfo("HR", "DEPARTMENTS_SEQ", { increment: "10", cache: "0", last_number: "280", max: "9990" }),
    "HR.AUDIT_SEQ": seqInfo("HR", "AUDIT_SEQ", { cache: "100", last_number: "5400", cycle: true }),
  },
  data: {
    EMPLOYEES: {
      columns: cols("EMPLOYEE_ID", "FIRST_NAME", "LAST_NAME", "EMAIL", "SALARY", "DEPARTMENT_ID"), total: 107,
      row: (i) => [String(100 + i), ["Ann", "Raj", "Mei", "Luis"][i % 4], `Lastname${i}`, `user${i}@corp.com`, String(4000 + ((i * 137) % 9000)), i % 9 === 0 ? null : String(10 * (1 + (i % 11)))],
    },
    DEPARTMENTS: { columns: cols("DEPARTMENT_ID", "DEPARTMENT_NAME"), total: 27, row: (i) => [String(10 * (i + 1)), `Dept ${i + 1}`] },
    JOB_HISTORY: { columns: cols("EMPLOYEE_ID", "START_DATE", "END_DATE"), total: 10, row: (i) => [String(100 + i), "2020-01-01", "2021-06-30"] },
    BIG_TABLE: {
      columns: cols("ID", "PAYLOAD", "CREATED"), total: 250_000,
      row: (i) => [String(i + 1), `row-${i + 1}-${"x".repeat(i % 20)}`, `2026-01-${String((i % 28) + 1).padStart(2, "0")}`],
    },
  },
};

// --------------------------------------------------------------- SALES (prod)
const salesOrders: TableInsight = {
  owner: "SALES", table: "ORDERS", comment: "Customer orders",
  columns: [
    ci("ORDER_ID", "NUMBER(12)", {
      nullable: false, primary_key: true, distinct: 120000, nulls: 0, comment: "Generated by identity",
      indexes: [{ name: "ORDERS_PK", position: 1, unique: true, leading: true, valid: true }],
      sequences: [{ owner: "SALES", name: "ISEQ$$_90210", via: "identity", via_name: "" }],
    }),
    ci("CUSTOMER_ID", "NUMBER(10)", {
      nullable: false, distinct: 9800, nulls: 0,
      foreign_keys: [{ constraint: "ORD_CUST_FK", ref_owner: "SALES", ref_table: "CUSTOMERS", ref_column: "CUSTOMER_ID" }],
      indexes: [{ name: "ORD_CUST_STATUS_IX", position: 1, unique: false, leading: true, valid: true }],
    }),
    ci("STATUS", "VARCHAR2(10)", { distinct: 5, nulls: 0, histogram: "FREQUENCY", checks: ["status IN ('NEW','PAID','SHIPPED','CANCELLED')"], indexes: [{ name: "ORD_CUST_STATUS_IX", position: 2, unique: false, leading: false, valid: true }] }),
    ci("TOTAL", "NUMBER(12,2)", { distinct: 40000, nulls: 12 }),
  ],
  indexes: [
    { name: "ORDERS_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["ORDER_ID"] },
    { name: "ORD_CUST_STATUS_IX", unique: false, valid: true, index_type: "NORMAL", columns: ["CUSTOMER_ID", "STATUS"] },
  ],
  triggers: [trg("SALES", "ORD_STATUS_TRG", "AFTER (row)", "UPDATE", { columns: ["STATUS"] })],
  sequences: [seq("SALES", "ISEQ$$_90210", "identity", "", ["ORDER_ID"], { generation: "ALWAYS", min: null, max: null, increment: null, cache: null, last_number: null })],
  referenced_by: [], uses: [{ owner: "SALES", name: "CUSTOMERS", object_type: "TABLE" }], used_by: [],
};
const salesCust: TableInsight = {
  owner: "SALES", table: "CUSTOMERS", comment: "Registered customers",
  columns: [
    ci("CUSTOMER_ID", "NUMBER(10)", {
      nullable: false, primary_key: true, distinct: 9800, nulls: 0,
      indexes: [{ name: "CUST_PK", position: 1, unique: true, leading: true, valid: true }],
      sequences: [{ owner: "SALES", name: "CUSTOMER_SEQ", via: "trigger", via_name: "CUST_BI" }],
      triggers: [{ name: "CUST_BI", timing: "BEFORE (row)", event: "INSERT", enabled: true, reason: "referenced" }],
    }),
    ci("NAME", "VARCHAR2(80)", { nullable: false, distinct: 9700, nulls: 0 }),
    ci("COUNTRY", "VARCHAR2(2)", { distinct: 54, nulls: 3, histogram: "TOP-FREQUENCY" }),
  ],
  indexes: [{ name: "CUST_PK", unique: true, valid: true, index_type: "NORMAL", columns: ["CUSTOMER_ID"] }],
  triggers: [trg("SALES", "CUST_BI", "BEFORE (row)", "INSERT", { columns: ["CUSTOMER_ID"], sequences: ["CUSTOMER_SEQ"] })],
  sequences: [seq("SALES", "CUSTOMER_SEQ", "trigger", "CUST_BI", ["CUSTOMER_ID"], { last_number: "9801" })],
  referenced_by: [{ owner: "SALES", table: "ORDERS", constraint: "ORD_CUST_FK" }], uses: [], used_by: [],
};
const sales: Db = {
  user: "SALES",
  schemas: ["SALES"],
  objects: {
    SALES: {
      TABLE: [entry("CUSTOMERS"), entry("ORDERS")],
      VIEW: [entry("OPEN_ORDERS")],
      INDEX: [
        entry("CUST_PK", { parent: "CUSTOMERS", detail: "UNIQUE · NORMAL · on CUSTOMERS" }),
        entry("ORD_CUST_STATUS_IX", { parent: "ORDERS", detail: "NONUNIQUE · NORMAL · on ORDERS" }),
        entry("ORDERS_PK", { parent: "ORDERS", detail: "UNIQUE · NORMAL · on ORDERS" }),
      ],
      SEQUENCE: [entry("CUSTOMER_SEQ", { detail: "+1 · last 9801" })],
      TRIGGER: [entry("CUST_BI", { parent: "CUSTOMERS", detail: "BEFORE EACH ROW · INSERT · on CUSTOMERS" }), entry("ORD_STATUS_TRG", { parent: "ORDERS", detail: "AFTER EACH ROW · UPDATE · on ORDERS" })],
      PROCEDURE: [entry("CLOSE_ORDER")],
    },
  },
  insights: { "SALES.ORDERS": salesOrders, "SALES.CUSTOMERS": salesCust },
  sources: {
    "SALES.CUST_BI.TRIGGER": "TRIGGER cust_bi\nBEFORE INSERT ON customers\nFOR EACH ROW\nBEGIN\n  SELECT customer_seq.NEXTVAL INTO :new.customer_id FROM dual;\nEND;",
    "SALES.ORD_STATUS_TRG.TRIGGER": "TRIGGER ord_status_trg\nAFTER UPDATE OF status ON orders\nFOR EACH ROW\nBEGIN\n  NULL; -- notify fulfilment\nEND;",
    "SALES.CLOSE_ORDER.PROCEDURE": "PROCEDURE close_order(p_id NUMBER) IS\nBEGIN\n  UPDATE orders SET status = 'SHIPPED' WHERE order_id = p_id;\nEND;",
  },
  seqs: { "SALES.CUSTOMER_SEQ": seqInfo("SALES", "CUSTOMER_SEQ", { last_number: "9801" }) },
  data: {
    ORDERS: { columns: cols("ORDER_ID", "CUSTOMER_ID", "STATUS", "TOTAL"), total: 1200, row: (i) => [String(5000 + i), String(100 + (i % 97)), ["NEW", "PAID", "SHIPPED"][i % 3], (50 + ((i * 31) % 900)).toFixed(2)] },
    CUSTOMERS: { columns: cols("CUSTOMER_ID", "NAME", "COUNTRY"), total: 300, row: (i) => [String(100 + i), `Customer ${i}`, ["US", "DE", "IN", "BR"][i % 4]] },
  },
};

// --------------------------------------------------------------------- plumbing
const dbs: Record<string, Db> = { FREEPDB1: hr, SALESPDB: sales };
const conns = new Map<string, Db>();
const connMeta = new Map<string, { key: string; name: string }>();
let nextConn = 1;
const db = (conn: string): Db => {
  const d = conns.get(conn);
  if (!d) throw `not connected (${conn})`;
  return d;
};

const ddlFor = (d: Db, owner: string, name: string, kind: string): string => {
  const t = d.insights[`${owner}.${name}`];
  if (kind === "TABLE" && t) {
    const body = t.columns.map((c) => `  "${c.name}" ${c.data_type}${c.nullable ? "" : " NOT NULL"}`).join(",\n");
    const pk = t.columns.filter((c) => c.primary_key).map((c) => `"${c.name}"`);
    return `CREATE TABLE "${owner}"."${name}"\n(\n${body}${pk.length ? `,\n  CONSTRAINT "${name}_PK" PRIMARY KEY (${pk.join(", ")})` : ""}\n);`;
  }
  const src = d.sources[`${owner}.${name}.${kind}`];
  if (src) return `CREATE OR REPLACE ${src}`;
  if (kind === "SEQUENCE") { const s = d.seqs[`${owner}.${name}`]; if (s) return `CREATE SEQUENCE "${owner}"."${name}" START WITH ${s.last_number} INCREMENT BY ${s.increment} CACHE ${s.cache}${s.cycle ? " CYCLE" : " NOCYCLE"};`; }
  if (kind === "INDEX") { const p = Object.values(d.insights).flatMap((x) => x.indexes.map((i) => ({ x, i }))).find((e) => e.i.name === name); if (p) return `CREATE ${p.i.unique ? "UNIQUE " : ""}INDEX "${owner}"."${name}" ON "${owner}"."${p.x.table}" (${p.i.columns.map((c) => `"${c}"`).join(", ")});`; }
  return `-- DDL for ${kind} ${owner}.${name} is not available in the demo`;
};

function analyze(sql: string): Analysis {
  const s = sql.trim().replace(/;$/, "");
  const l = s.toLowerCase();
  const kind = l.startsWith("select") || l.startsWith("with") ? "select" : l.split(/\s+/)[0] || "empty";
  const warnings: Analysis["warnings"] = [];
  if ((kind === "update" || kind === "delete") && !/\bwhere\b/.test(l)) warnings.push({ severity: "danger", message: "No WHERE clause: this will affect every row in the table." });
  if (kind === "drop" || kind === "truncate") warnings.push({ severity: "danger", message: "DROP/TRUNCATE is permanent and cannot be rolled back." });
  return { kind, sql: s, warnings, requires_confirmation: warnings.some((w) => w.severity === "danger"), blocked: null, read_only: kind === "select" };
}

const cursors = new Map<number, { table: Data; pos: number }>();
let nextHandle = 1;
const page = (t: Data, from: number, n: number): Row[] => Array.from({ length: Math.max(0, Math.min(n, t.total - from)) }, (_, k) => t.row(from + k));

function rowsResult(t: Data, analysis: Analysis, pageSize: number): RunResult {
  const rows = page(t, 0, pageSize);
  const done = rows.length >= t.total;
  const handle = nextHandle++;
  if (!done) cursors.set(handle, { table: t, pos: rows.length });
  return { kind: "rows", handle, columns: t.columns, rows, done, analysis };
}

function mockRefs(sql: string): TableRef[] {
  const out: TableRef[] = [];
  const re = /\b(?:from|join)\s+(?:(\w+)\.)?(\w+)(?:\s+(?:as\s+)?(?!where|join|on|left|right|inner|group|order\b)(\w+))?/gi;
  for (const m of sql.matchAll(re)) out.push({ schema: m[1]?.toUpperCase() ?? null, name: m[2].toUpperCase(), alias: m[3]?.toUpperCase() ?? null });
  return out;
}

function mockComplete(d: Db, sql: string, cursor: number, owner: string): Completions {
  const before = sql.slice(0, cursor);
  const word = /[\w$#]*$/.exec(before)![0];
  const from = cursor - word.length;
  const refs = mockRefs(sql);
  const wl = word.toUpperCase();
  const hit = (l: string) => l.toUpperCase().includes(wl);
  const colsOf = (o: string, t: string) => d.insights[`${o}.${t}`]?.columns.map((c) => c.name) ?? d.data[t]?.columns.map((c) => c.name) ?? [];
  const items: Suggestion[] = [];
  const dot = /(\w+)\.$/.exec(before.slice(0, from));
  if (dot) {
    const q = dot[1].toUpperCase();
    const r = refs.find((x) => x.alias === q || x.name === q);
    for (const c of colsOf(r?.schema ?? owner, r?.name ?? q)) if (hit(c)) items.push({ label: c, insert: c, kind: "column", detail: "column", score: 100 });
  } else if (/\b(from|join)\s+(\w*)$/i.test(before)) {
    for (const t of (d.objects[owner]?.TABLE ?? []).map((e) => e.name)) if (hit(t)) items.push({ label: t, insert: t, kind: "table", detail: null, score: 50 });
  } else {
    for (const r of refs) for (const c of colsOf(r.schema ?? owner, r.name)) if (hit(c)) items.push({ label: c, insert: refs.length > 1 ? `${r.alias ?? r.name}.${c}` : c, kind: "column", detail: r.name, score: 100 });
  }
  return { from, items };
}

function corpus(d: Db): Hit[] {
  const out: Hit[] = [];
  for (const [owner, kinds] of Object.entries(d.objects)) {
    for (const [k, list] of Object.entries(kinds)) {
      const kind = k === "TABLE" ? "table" : k === "VIEW" ? "view" : null;
      if (kind) for (const e of list ?? []) out.push({ kind, owner, name: e.name, parent: "", line: null, snippet: d.insights[`${owner}.${e.name}`]?.comment ?? "", score: -5 });
    }
  }
  for (const t of Object.values(d.insights)) for (const c of t.columns) out.push({ kind: "column", owner: t.owner, name: c.name, parent: t.table, line: null, snippet: c.comment ?? "", score: -4 });
  for (const [key, src] of Object.entries(d.sources)) {
    const [owner, name, ...rest] = key.split(".");
    src.split("\n").forEach((line, i) => line.trim() && out.push({ kind: "source", owner, name, parent: rest.join("."), line: i + 1, snippet: line.trim().replace(/\b(\w+)\b/g, "$1"), score: -2 }));
  }
  return out;
}

const typeParts = (display: string): Pick<import("./types").TargetCol, "data_type" | "char_length" | "data_length" | "char_used" | "precision" | "scale"> => {
  const m = /^(\w+)(?:\((\d+)(?:,(\d+))?(?: CHAR)?\))?/.exec(display) ?? [];
  const base = m[1] ?? display;
  return { data_type: base, char_length: base === "VARCHAR2" ? Number(m[2] ?? 0) : 0, data_length: base === "NUMBER" ? 22 : Number(m[2] ?? 7), char_used: base === "VARCHAR2" ? "C" : "", precision: base === "NUMBER" && m[2] ? Number(m[2]) : null, scale: base === "NUMBER" && m[2] ? Number(m[3] ?? 0) : null };
};

const transfer = transferApi({
  tableRows(conn, source) {
    const d = db(conn);
    const sql = source.type === "query" ? source.sql : "";
    const name = source.type === "table" ? source.table : Object.keys(d.data).find((k) => new RegExp(`\\b${k}\\b`, "i").test(sql));
    const t = name ? d.data[name.toUpperCase()] : undefined;
    if (!t) throw "ORA-00942: table or view does not exist";
    const owner = source.type === "table" ? source.owner : Object.keys(d.objects)[0];
    const ins = d.insights[`${owner}.${name!.toUpperCase()}`];
    const n = Math.min(t.total, 5000);
    return { columns: t.columns.map((c) => c.name), types: t.columns.map((c) => ins?.columns.find((x) => x.name === c.name)?.data_type ?? "VARCHAR2(30)"), rows: Array.from({ length: n }, (_, i) => t.row(i)) };
  },
  targets(conn, owner, table) {
    const d = db(conn);
    const ins = d.insights[`${owner}.${table}`];
    if (!ins) throw `table ${owner}.${table} was not found or is not visible to this user`;
    return ins.columns.map((c) => ({ name: c.name, display: c.data_type, nullable: c.nullable, insertable: true, note: null, ...typeParts(c.data_type) }));
  },
});

export const mockApi: Api = {
  ...transfer,
  ...graphApi(),
  historyStatus: () => mockHistory.status(),
  historySetEnabled: (e) => mockHistory.setEnabled(e),
  historySearch: (q) => mockHistory.search(q),
  historySuggest: (t, k, l) => mockHistory.suggest(t, k, l),
  historyPin: (h, p) => mockHistory.pin(h, p),
  historyForget: (h) => mockHistory.forget(h),
  historyPurge: (b) => mockHistory.purge(b),
  wsSave: (w) => mockHistory.wsSave(w),
  wsClose: (u) => mockHistory.wsClose(u),
  wsPin: (u, p) => mockHistory.wsPin(u, p),
  wsDelete: (u) => mockHistory.wsDelete(u),
  wsList: (f) => mockHistory.wsList(f),
  wsGet: (u) => mockHistory.wsGet(u),
  wsSteps: (u) => mockHistory.wsSteps(u),
  listProfiles: async (): Promise<Profile[]> => [
    { name: "HR · development", host: "dev-db.corp", port: 1521, service: "FREEPDB1", user: "hr", environment: "dev" },
    { name: "SALES · production", host: "prod-db.corp", port: 1521, service: "SALESPDB", user: "sales_ro", environment: "prod" },
  ],
  saveProfile: async () => {},
  deleteProfile: async () => {},
  async connect(profile): Promise<ConnInfo> {
    await sleep(250);
    const d = dbs[profile.service];
    if (!d) throw "ORA-12514: listener does not currently know of service requested";
    const id = `m${nextConn++}`;
    conns.set(id, d);
    connMeta.set(id, { key: `key-${profile.service}`, name: profile.name });
    return { id, key: `key-${profile.service}` };
  },
  disconnect: async (c) => void conns.delete(c),
  async runQuery(conn, sql, confirmed, pageSize, owner, worksheet) {
    const d = db(conn);
    const meta = connMeta.get(conn)!;
    const t0 = performance.now();
    const rec = (status: "ok" | "error", rows: number | null, error: string | null) =>
      record({ sql, conn_key: meta.key, conn_name: meta.name, owner, worksheet, at: Date.now(), ms: Math.round(performance.now() - t0) + 12, rows, status, error });
    await sleep(60);
    const analysis = analyze(sql);
    if (analysis.requires_confirmation && !confirmed) return { kind: "needs_confirmation", analysis };
    if (analysis.kind !== "select") { rec("ok", 3, null); return { kind: "affected", count: 3, analysis }; }
    const name = Object.keys(d.data).find((k) => new RegExp(`\\b${k}\\b`, "i").test(analysis.sql));
    if (!name) { rec("error", null, "ORA-00942: table or view does not exist"); throw "ORA-00942: table or view does not exist"; }
    const r = rowsResult(d.data[name], analysis, pageSize);
    rec("ok", r.kind === "rows" ? r.rows.length : null, null);
    return r;
  },
  async fetchMore(_c, handle, pageSize) {
    await sleep(40);
    const c = cursors.get(handle);
    if (!c) throw `unknown query handle ${handle}`;
    const rows = page(c.table, c.pos, pageSize);
    c.pos += rows.length;
    const done = c.pos >= c.table.total;
    if (done) cursors.delete(handle);
    return { rows, done, truncated: false };
  },
  closeQuery: async (_c, h) => void cursors.delete(h),
  cancelQuery: async () => {},
  commit: async () => {},
  rollback: async () => {},
  listSchemas: async (c) => db(c).schemas,
  async objectCounts(c, owner) {
    await sleep(40);
    return Object.fromEntries(Object.entries(db(c).objects[owner] ?? {}).map(([k, v]) => [k, v?.length ?? 0]).filter(([, n]) => (n as number) > 0));
  },
  async listObjects(c, owner, kind) {
    await sleep(40);
    return db(c).objects[owner]?.[kind as ObjectKind] ?? [];
  },
  objectSource: async (c, owner, name, kind) => db(c).sources[`${owner}.${name}.${kind}`] ?? "",
  objectDdl: async (c, owner, name, kind) => ddlFor(db(c), owner, name, kind),
  sequenceInfo: async (c, owner, name) => db(c).seqs[`${owner}.${name}`] ?? null,
  async objectDependencies(c, _owner, name) {
    const d = db(c);
    const uses: ObjectDeps["uses"] = [];
    const used_by: ObjectDeps["used_by"] = [];
    const nameRe = new RegExp(`\\b${name}\\b`, "i");
    for (const [key, src] of Object.entries(d.sources)) {
      const [o, n, ...kind] = key.split(".");
      if (n === name) {
        for (const t of Object.keys(d.data)) if (new RegExp(`\\b${t}\\b`, "i").test(src)) uses.push({ owner: o, name: t, object_type: "TABLE" });
        for (const sq of Object.keys(d.seqs)) { const sn = sq.split(".")[1]; if (new RegExp(`\\b${sn}\\b`, "i").test(src)) uses.push({ owner: o, name: sn, object_type: "SEQUENCE" }); }
      } else if (nameRe.test(src)) used_by.push({ owner: o, name: n, object_type: kind.join(".") });
    }
    for (const t of Object.values(d.insights)) if (t.sequences.some((s) => s.name === name) && !used_by.some((u) => u.name === t.table)) used_by.push({ owner: t.owner, name: t.table, object_type: "TABLE" });
    return { uses, used_by };
  },
  async previewTable(c, owner, table, limit) {
    const d = db(c);
    const t = d.data[table];
    if (!t) throw `ORA-00942: table or view "${owner}"."${table}" does not exist`;
    return rowsResult(t, analyze(`SELECT * FROM "${owner}"."${table}" WHERE ROWNUM <= ${limit}`), limit);
  },
  async refreshMetadata(c, owner) {
    await sleep(80);
    const o = db(c).objects[owner] ?? {};
    return { tables: o.TABLE?.length ?? 0, columns: 10, indexed_docs: 13 };
  },
  async tableInsight(c, owner, table) {
    await sleep(40);
    const d = db(c);
    return d.insights[`${owner}.${table}`] ?? { owner, table, comment: null, columns: (d.data[table]?.columns ?? cols("ID")).map((x) => ci(x.name, "VARCHAR2(30)")), indexes: [], triggers: [], sequences: [], referenced_by: [], uses: [], used_by: [] };
  },
  complete: async (c, sql, cursor, owner) => mockComplete(db(c), sql, cursor, owner),
  references: async (sql) => mockRefs(sql),
  async statements(sql) {
    const out: { start: number; end: number; kind: string; text: string }[] = [];
    let pos = 0;
    for (const part of sql.split(";")) {
      const start = pos;
      pos += part.length + 1;
      const text = part.trim();
      if (!text.replace(/--.*$/gm, "").trim()) continue;
      const lead = part.length - part.trimStart().length;
      out.push({ start: start + lead, end: start + lead + text.length, kind: /^\s*(\w+)/.exec(text)?.[1]?.toLowerCase() ?? "other", text });
    }
    return out;
  },
  async searchObjects(c, text, owner, kinds, limit) {
    const words = text.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
    if (!words.length) return [];
    const hay = (h: Hit) => `${h.name} ${h.parent} ${h.snippet}`.toLowerCase();
    return corpus(db(c))
      .filter((h) => words.every((w) => hay(h).split(/[^a-z0-9]+/).some((t) => t.startsWith(w))) && (!owner || h.owner === owner) && (!kinds.length || kinds.includes(h.kind)))
      .sort((a, b) => Number(b.name.toLowerCase() === text.toLowerCase()) - Number(a.name.toLowerCase() === text.toLowerCase()) || a.score - b.score)
      .slice(0, limit);
  },
  previewComment: async (owner, table, column, text) =>
    `COMMENT ON ${column ? `COLUMN "${owner}"."${table}"."${column}"` : `TABLE "${owner}"."${table}"`} IS '${text.replace(/'/g, "''")}'`,
  saveComment: async (c, owner, table, column, text) => {
    const t = db(c).insights[`${owner}.${table}`];
    if (t) { if (column) { const x = t.columns.find((y) => y.name === column); if (x) x.comment = text || null; } else t.comment = text || null; }
    return "ok";
  },
};
