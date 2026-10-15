// Demo-only stand-in for the Rust comparison engines (ora-core `textdiff`, `datadiff`, `schemadiff`),
// so the UI can be built and checked in a browser. The real logic and its tests live in ora-core.
import type { Api } from "./api";
import { mockFileText, parseCsv, runJob } from "./mock-transfer";
import type { CellDiff, ChangedRow, DataDiffOptions, DataDiffReport, DataSide, DiffRow, DiffSpan, SchemaDiff, TextOptions } from "./types";

// ------------------------------------------------------------------------------------ text
const linesOf = (t: string) => (t ? t.replace(/\n$/, "").split("\n").map((l) => l.replace(/\r$/, "")) : []);
const keyOf = (l: string, o: TextOptions) => { let k = l; if (o.ignore_whitespace) k = k.split(/\s+/).filter(Boolean).join(" "); if (o.ignore_case) k = k.toLowerCase(); if (o.ignore_sql_comments) k = k.trimEnd(); return k; };

function lcs<T>(a: T[], b: T[], eq: (x: T, y: T) => boolean): [number, number][] {
  const n = a.length, m = b.length;
  const d = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) d[i][j] = eq(a[i], b[j]) ? d[i + 1][j + 1] + 1 : Math.max(d[i + 1][j], d[i][j + 1]);
  const out: [number, number][] = []; let i = 0, j = 0;
  while (i < n && j < m) { if (eq(a[i], b[j])) { out.push([i, j]); i++; j++; } else if (d[i + 1][j] >= d[i][j + 1]) i++; else j++; }
  return out;
}

function spans(a: string, b: string): [DiffSpan[], DiffSpan[]] {
  const ta = a.split(/(\s+)/).filter((x) => x !== ""), tb = b.split(/(\s+)/).filter((x) => x !== "");
  const pairs = lcs(ta, tb, (x, y) => x === y);
  const sa: DiffSpan[] = [], sb: DiffSpan[] = [];
  const push = (v: DiffSpan[], text: string, changed: boolean) => { const l = v[v.length - 1]; if (l && l.changed === changed) l.text += text; else v.push({ text, changed }); };
  let i = 0, j = 0;
  for (const [pi, pj] of [...pairs, [ta.length, tb.length] as [number, number]]) {
    while (i < pi) push(sa, ta[i++], true);
    while (j < pj) push(sb, tb[j++], true);
    if (pi < ta.length) { push(sa, ta[pi], false); push(sb, tb[pj], false); i = pi + 1; j = pj + 1; }
  }
  return [sa, sb];
}

function textDiff(a: string, b: string, o: TextOptions, context: number | null) {
  const strip = (t: string) => (o.ignore_sql_comments ? t.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " ")).replace(/--[^\n]*/g, "") : t);
  const la = linesOf(a), lb = linesOf(b);
  const ka = linesOf(strip(a)).map((l) => keyOf(l, o)), kb = linesOf(strip(b)).map((l) => keyOf(l, o));
  const pairs = lcs(ka, kb, (x, y) => x === y);
  const rows: DiffRow[] = [];
  const row = (kind: DiffRow["kind"], i: number | null, j: number | null): DiffRow => ({ kind, a_no: i === null ? null : i + 1, b_no: j === null ? null : j + 1, a: i === null ? null : la[i], b: j === null ? null : lb[j], a_spans: [], b_spans: [], skipped: 0 });
  let i = 0, j = 0;
  const block = (ei: number, ej: number) => {
    const n = Math.min(ei - i, ej - j);
    for (let k = 0; k < n; k++) { const r = row("change", i + k, j + k); [r.a_spans, r.b_spans] = spans(la[i + k], lb[j + k]); rows.push(r); }
    for (let k = n; k < ei - i; k++) rows.push(row("delete", i + k, null));
    for (let k = n; k < ej - j; k++) rows.push(row("insert", null, j + k));
  };
  for (const [pi, pj] of pairs) { block(pi, pj); rows.push(row("equal", pi, pj)); i = pi + 1; j = pj + 1; }
  block(la.length, lb.length);
  const stats = { equal: rows.filter((r) => r.kind === "equal").length, deleted: rows.filter((r) => r.kind === "delete").length, inserted: rows.filter((r) => r.kind === "insert").length, changed: rows.filter((r) => r.kind === "change").length, lines_a: la.length, lines_b: lb.length, identical: false, byte_identical: a === b };
  stats.identical = stats.deleted + stats.inserted + stats.changed === 0;
  if (context === null) return { rows, stats };
  const near = rows.map(() => false);
  rows.forEach((r, k) => { if (r.kind !== "equal") for (let x = Math.max(0, k - context); x <= Math.min(rows.length - 1, k + context); x++) near[x] = true; });
  const out: DiffRow[] = []; let skipped = 0;
  const flush = () => { if (skipped) { out.push({ kind: "skip", a_no: null, b_no: null, a: null, b: null, a_spans: [], b_spans: [], skipped }); skipped = 0; } };
  rows.forEach((r, k) => { if (near[k]) { flush(); out.push(r); } else skipped++; });
  flush();
  return { rows: out, stats };
}

// ------------------------------------------------------------------------------------ data
const canon = (v: string | null, o: DataDiffOptions): string | null => {
  if (v === null) return null;
  let s = o.trim ? v.trim() : v;
  if (s === "" && o.empty_is_null) return null;
  if (o.numeric && /^[+-]?\d+(\.\d+)?$/.test(s) && !/^[+-]?0\d/.test(s)) return "n:" + String(Number(s));
  if (o.dates && /^\d{4}-\d{2}-\d{2}([ T]\d{2}:\d{2}(:\d{2})?)?$/.test(s)) { const t = s.replace("T", " "); const [d, tm = "00:00:00"] = t.split(" "); return "d:" + d + " " + (tm.length === 5 ? tm + ":00" : tm); }
  if (o.ignore_case) s = s.toLowerCase();
  return "t:" + s;
};

export interface CompareDeps { rows(side: DataSide): { columns: string[]; rows: (string | null)[][] } }

export function compareApi(deps: CompareDeps): Pick<Api, "textDiff" | "textPatch" | "textReadFile" | "textWriteFile" | "dataSidePreview" | "dataDiffStart" | "schemaCompare"> {
  const load = (side: DataSide) => {
    if (side.type === "file") {
      const text = mockFileText(side.path);
      if (text === undefined) throw `${side.path} is not a file`;
      const { rows } = parseCsv(text);
      return { columns: rows[0] ?? [], rows: rows.slice(1) as (string | null)[][] };
    }
    return deps.rows(side);
  };
  return {
    textDiff: async (a, b, o, ctx) => textDiff(a, b, o, ctx),
    textPatch: async (a, b, o, an, bn) => { const r = textDiff(a, b, o, 3); return r.stats.identical ? "" : `--- ${an}\n+++ ${bn}\n` + r.rows.map((x) => (x.kind === "equal" ? ` ${x.a}` : x.kind === "delete" ? `-${x.a}` : x.kind === "insert" ? `+${x.b}` : x.kind === "change" ? `-${x.a}\n+${x.b}` : "")).filter(Boolean).join("\n") + "\n"; },
    textReadFile: async (path) => { const t = mockFileText(path); if (t === undefined) throw `${path} is not a file`; return t; },
    textWriteFile: async (path, content) => { const a = document.createElement("a"); a.href = URL.createObjectURL(new Blob([content], { type: "text/plain" })); a.download = path.split("/").pop() ?? "file.txt"; a.click(); setTimeout(() => URL.revokeObjectURL(a.href), 1000); },
    dataSidePreview: async (side) => {
      const d = load(side);
      const uniq = d.columns.map((_, i) => new Set(d.rows.map((r) => r[i])).size === d.rows.length && d.rows.every((r) => (r[i] ?? "") !== ""));
      const idCol = d.columns.findIndex((c, i) => uniq[i] && /id$/i.test(c));
      const first = idCol >= 0 ? idCol : uniq.indexOf(true);
      return { columns: d.columns, sample: d.rows.slice(0, 8), suggested_keys: first >= 0 ? [[d.columns[first]]] : [] };
    },
    dataDiffStart: async (a, b, o, outPath) => {
      const da = load(a), db = load(b);
      return runJob("compare", da.rows.length + db.rows.length, () => {
        const norm = (s: string) => s.trim().toLowerCase();
        const common = da.columns.map((c, i) => ({ name: c, ia: i, ib: db.columns.findIndex((x) => norm(x) === norm(c)) })).filter((c) => c.ib >= 0);
        if (!common.length) throw "the two sides have no column name in common";
        const keyNames = o.keys.length ? o.keys : common.map((c) => c.name);
        const keys = keyNames.map((k) => { const c = common.find((x) => norm(x.name) === norm(k)); if (!c) throw `key column "${k}" is not in both sides`; return c; });
        const rest = common.filter((c) => !keys.includes(c));
        const whole = o.keys.length === 0;
        const keyOfRow = (r: (string | null)[], side: "ia" | "ib") => keys.map((k) => canon(r[k[side]] ?? null, o));
        const rep: DataDiffReport = { key_columns: keys.map((k) => k.name), whole_row: whole, compared_columns: common.map((c) => c.name), only_in_a_columns: da.columns.filter((c) => !common.some((x) => x.name === c)), only_in_b_columns: db.columns.filter((c) => !common.some((x) => norm(x.name) === norm(c))), rows_a: da.rows.length, rows_b: db.rows.length, same: 0, changed: 0, only_a: 0, only_b: 0, duplicate_keys_a: 0, duplicate_keys_b: 0, empty_keys_a: 0, empty_keys_b: 0, by_column: [], layout: [...keys, ...rest].map((c) => c.name), changed_samples: [], only_a_samples: [], only_b_samples: [], diff_file: outPath, spilled: false, cancelled: false, identical: false, elapsed_ms: 420, warnings: [] };
        const map = new Map<string, { row: (string | null)[]; matched: boolean }>();
        for (const r of da.rows) { const k = keyOfRow(r, "ia"); if (k.some((x) => x === null)) { rep.empty_keys_a++; rep.only_a++; continue; } const ks = k.join("\u001f"); if (map.has(ks)) rep.duplicate_keys_a++; else map.set(ks, { row: r, matched: false }); }
        const byCol = new Map<string, number>();
        const lay = (r: (string | null)[], side: "ia" | "ib") => [...keys, ...rest].map((c) => r[c[side]] ?? null);
        for (const r of db.rows) {
          const k = keyOfRow(r, "ib");
          if (k.some((x) => x === null)) { rep.empty_keys_b++; rep.only_b++; continue; }
          const e = map.get(k.join("\u001f"));
          if (!e) { rep.only_b++; if (rep.only_b_samples.length < o.max_samples) rep.only_b_samples.push(lay(r, "ib")); continue; }
          if (e.matched) { rep.duplicate_keys_b++; continue; }
          e.matched = true;
          const diffs: CellDiff[] = rest.filter((c) => canon(e.row[c.ia] ?? null, o) !== canon(r[c.ib] ?? null, o)).map((c) => ({ column: c.name, a: e.row[c.ia] ?? null, b: r[c.ib] ?? null }));
          if (!diffs.length) rep.same++;
          else { rep.changed++; diffs.forEach((d) => byCol.set(d.column, (byCol.get(d.column) ?? 0) + 1)); if (rep.changed_samples.length < o.max_samples) rep.changed_samples.push({ key: keys.map((c) => e.row[c.ia] ?? null), diffs } as ChangedRow); }
        }
        for (const e of map.values()) if (!e.matched) { rep.only_a++; if (rep.only_a_samples.length < o.max_samples) rep.only_a_samples.push(lay(e.row, "ia")); }
        rep.by_column = [...byCol].sort((x, y) => y[1] - x[1]);
        rep.identical = rep.changed + rep.only_a + rep.only_b === 0 && rep.rows_a === rep.rows_b;
        if (rep.duplicate_keys_a + rep.duplicate_keys_b) rep.warnings.push(`the key is not unique (${rep.duplicate_keys_a} repeated key(s) in A, ${rep.duplicate_keys_b} in B): only the first row of each key was compared; choose more key columns`);
        if (rep.only_in_a_columns.length || rep.only_in_b_columns.length) rep.warnings.push(`columns not compared because only one side has them: A only [${rep.only_in_a_columns.join(", ")}], B only [${rep.only_in_b_columns.join(", ")}]`);
        return rep;
      });
    },
    schemaCompare: async (_ca, oa, _cb, ob, includeCode) => {
      await new Promise((r) => setTimeout(r, 300));
      const d: SchemaDiff = {
        owner_a: oa, owner_b: ob, tables_only_a: ["PAYSLIPS"], tables_only_b: ["OLD_AUDIT"],
        tables: [{ name: "EMPLOYEES", changes: [
          { change: "only_a", what: "column", name: "EMAIL", a: "VARCHAR2(80) NOT NULL", b: null },
          { change: "differs", what: "column", name: "NAME", a: "VARCHAR2(40 CHAR) NULL", b: "VARCHAR2(20 CHAR) NULL" },
          { change: "only_b", what: "column", name: "LEGACY_CODE", a: null, b: "NUMBER NULL" },
          { change: "only_a", what: "index", name: "EMP_DEPT_IX", a: "non-unique (DEPARTMENT_ID)", b: null },
          { change: "only_a", what: "foreign key", name: "EMP_DEPT_FK", a: "R:DEPARTMENT_ID -> DEPARTMENTS(DEPARTMENT_ID)", b: null },
        ] }],
        sequences: [{ change: "differs", what: "sequence", name: "EMPLOYEES_SEQ", a: "increment 1 min 1 max 999999 cache 20 nocycle", b: "increment 10 min 1 max 999999 cache 20 nocycle" }],
        code: includeCode ? [{ kind: "PACKAGE BODY", name: "EMP_PKG", change: "differs" }, { kind: "PROCEDURE", name: "ADD_JOB_HISTORY", change: "only_a" }] : [], code_compared: includeCode,
        summary: { tables_only_a: 1, tables_only_b: 1, tables_differ: 1, column_changes: 3, other_changes: 3, code_only_a: includeCode ? 1 : 0, code_only_b: 0, code_differs: includeCode ? 1 : 0, identical: false },
        script: `-- Draft: make ${ob} look like ${oa}. REVIEW BEFORE RUNNING; nothing here has been executed.\n-- Destructive statements are commented out. Defaults, storage, partitioning and grants are not included.\n\nALTER SEQUENCE "${ob}"."EMPLOYEES_SEQ" INCREMENT BY 1 MINVALUE 1 MAXVALUE 999999 NOCYCLE CACHE 20;\n\nCREATE TABLE "${ob}"."PAYSLIPS" (\n  "ID" NUMBER(10) NOT NULL,\n  CONSTRAINT "PAYSLIPS_PK" PRIMARY KEY ("ID")\n);\n\n-- EMPLOYEES\nALTER TABLE "${ob}"."EMPLOYEES" ADD ("EMAIL" VARCHAR2(80));\n-- after filling existing rows: ALTER TABLE "${ob}"."EMPLOYEES" MODIFY ("EMAIL" NOT NULL);\n-- review: fails if existing data does not fit\nALTER TABLE "${ob}"."EMPLOYEES" MODIFY ("NAME" VARCHAR2(40 CHAR));\nCREATE INDEX "${ob}"."EMP_DEPT_IX" ON "${ob}"."EMPLOYEES" ("DEPARTMENT_ID");\n\n-- Destructive: only in B. Uncomment deliberately.\n-- ALTER TABLE "${ob}"."EMPLOYEES" DROP COLUMN "LEGACY_CODE";\n-- DROP TABLE "${ob}"."OLD_AUDIT";\n`,
        notes: ["Demo data. A is the reference: the script makes B look like A.", "Not compared: default values, storage, partitioning, grants, comments, column order."],
      };
      return d;
    },
  };
}
