// Demo-only stand-in for the Rust export/import engine so the UI can be built and
// verified in a browser. The real logic (and its tests) lives in ora-core.
import type { Api } from "./api";
import type {
  ColProfile, ColumnMap, ExportReport, ExportSource, FileProfile, ImportReport, JobEvent, MappingPlan, PlanInput, Recommendation,
  Reject, TargetCol, ValidationReport,
} from "./types";

export interface TransferDeps {
  tableRows(conn: string, source: ExportSource): { columns: string[]; types: string[]; rows: (string | null)[][] };
  targets(conn: string, owner: string, table: string): TargetCol[];
}

const files = new Map<string, string>();
const listeners = new Set<(e: JobEvent) => void>();
const emit = (e: JobEvent) => listeners.forEach((l) => l(e));
const cancelled = new Set<string>();
let seq = 0;
const profiles = new Map<string, { profile: FileProfile; rows: string[][] }>();
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const cancelJob = (id: string) => void cancelled.add(id);
export const mockFileText = (path: string) => files.get(path);

// ---- tiny CSV reader (quotes, delimiter sniffing)
export function parseCsv(text: string): { rows: string[][]; delimiter: string } {
  const t = text.replace(/^﻿/, "");
  const first = t.split(/\r?\n/)[0] ?? "";
  const delimiter = [",", ";", "\t", "|"].map((d) => [d, first.split(d).length] as const).sort((a, b) => b[1] - a[1])[0][0];
  const rows: string[][] = [];
  let row: string[] = [], cur = "", q = false;
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (q) { if (c === '"' && t[i + 1] === '"') { cur += '"'; i++; } else if (c === '"') q = false; else cur += c; }
    else if (c === '"') q = true;
    else if (c === delimiter) { row.push(cur); cur = ""; }
    else if (c === "\n" || c === "\r") { if (c === "\r" && t[i + 1] === "\n") i++; row.push(cur); cur = ""; if (row.some((x) => x.trim() !== "") || row.length > 1) rows.push(row); row = []; }
    else cur += c;
  }
  if (cur !== "" || row.length) { row.push(cur); rows.push(row); }
  return { rows, delimiter };
}

const num = (s: string, dec: string) => {
  const t = s.trim();
  const re = dec === "." ? /^[+-]?(\d{1,3}(,\d{3})+|\d+)(\.\d+)?$/ : /^[+-]?(\d{1,3}(\.\d{3})+|\d+)(,\d+)?$/;
  return re.test(t) && !(/^[+-]?0\d/.test(t));
};
const canonNum = (s: string, dec: string) => s.trim().replace(dec === "." ? /,/g : /\./g, "").replace(",", ".").replace(/^\+/, "");
const FAMS: [string, RegExp, (m: RegExpExecArray) => string | null][] = [
  ["YYYY-MM-DD", /^(\d{4})-(\d{2})-(\d{2})(?: (\d{2}):(\d{2}):(\d{2}))?$/, (m) => iso(+m[1], +m[2], +m[3], m[4], m[5], m[6])],
  ["DD/MM/YYYY", /^(\d{1,2})\/(\d{1,2})\/(\d{4})(?: (\d{2}):(\d{2}):(\d{2}))?$/, (m) => iso(+m[3], +m[2], +m[1], m[4], m[5], m[6])],
  ["MM/DD/YYYY", /^(\d{1,2})\/(\d{1,2})\/(\d{4})(?: (\d{2}):(\d{2}):(\d{2}))?$/, (m) => iso(+m[3], +m[1], +m[2], m[4], m[5], m[6])],
];
function iso(y: number, mo: number, d: number, h?: string, mi?: string, s?: string): string | null {
  const dt = new Date(Date.UTC(y, mo - 1, d));
  if (dt.getUTCFullYear() !== y || dt.getUTCMonth() !== mo - 1 || dt.getUTCDate() !== d) return null;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${y}-${p(mo)}-${p(d)} ${h ?? "00"}:${mi ?? "00"}:${s ?? "00"}`;
}
const parseDate = (v: string, fam: string) => { const f = FAMS.find((x) => x[0] === fam); const m = f?.[1].exec(v.trim()); return m && f ? f[2](m) : null; };

function profileOf(name: string, text: string): FileProfile {
  const { rows, delimiter } = parseCsv(text);
  const head = rows[0] ?? [];
  const hasHeader = head.length > 0 && head.every((c) => c.trim() !== "" && !num(c, ".") && !FAMS.some((f) => f[1].test(c.trim())));
  const names = hasHeader ? head : head.map((_, i) => `COLUMN_${i + 1}`);
  const data = hasHeader ? rows.slice(1) : rows;
  const columns: ColProfile[] = names.map((n, i) => {
    const vals = data.map((r) => r[i] ?? "").filter((v) => v.trim() !== "");
    const fams = FAMS.filter((f) => vals.length && vals.every((v) => parseDate(v, f[0]))).map((f) => f[0]);
    const dotOk = vals.length > 0 && vals.every((v) => num(v, ".")), commaOk = vals.length > 0 && vals.every((v) => num(v, ","));
    const leading = vals.some((v) => /^[+-]?0\d/.test(v.trim()));
    const dec = dotOk ? "." : ",";
    const anySep = vals.some((v) => /[.,]/.test(v));
    const scale = Math.max(0, ...vals.map((v) => (canonNum(v, dec).split(".")[1] ?? "").length));
    const intDigits = Math.max(0, ...vals.map((v) => canonNum(v, dec).replace(/^[+-]/, "").split(".")[0].length));
    let kind: ColProfile["kind"] = vals.length === 0 ? "empty" : "text";
    if (vals.length && !leading && (dotOk || commaOk)) kind = scale === 0 ? "integer" : "decimal";
    else if (vals.length && fams.length) kind = "date";
    return {
      index: i, name: n.trim() || `COLUMN_${i + 1}`, kind, nulls: data.length - vals.length, non_null: vals.length,
      max_chars: Math.max(0, ...vals.map((v) => v.length)), max_bytes: Math.max(0, ...vals.map((v) => new Blob([v]).size)),
      int_digits: intDigits, scale, date_formats: kind === "date" ? fams : [], date_ambiguous: kind === "date" && fams.length > 1,
      number_style: kind === "integer" || kind === "decimal" ? { decimal: dec, thousands: null } : null,
      number_ambiguous: (dotOk && commaOk && anySep), leading_zeros: leading, samples: [...new Set(vals)].slice(0, 5),
    };
  });
  const warnings: string[] = [];
  if (columns.some((c) => c.date_ambiguous)) warnings.push("Some date columns could be day-first or month-first. Check the format chosen for each.");
  if (columns.some((c) => c.number_ambiguous)) warnings.push("Some numbers could use either '.' or ',' as the decimal mark. Check the style chosen for each.");
  profiles.set(name, { profile: { columns, rows_scanned: data.length, more_rows: false, ragged_rows: 0, format: "csv", delimiter, has_header: hasHeader, encoding: "UTF-8", sheets: [], warnings }, rows: data });
  return profiles.get(name)!.profile;
}

const norm = (s: string) => s.replace(/[^a-z0-9]/gi, "").toUpperCase();

function planOf(profile: FileProfile, targets: TargetCol[], owner: string, table: string, input: PlanInput): MappingPlan {
  const used = new Set<number>();
  const auto: Record<string, number | null> = {};
  for (const t of targets.filter((x) => x.insertable)) {
    const c = profile.columns.find((x) => !used.has(x.index) && norm(x.name) === norm(t.name));
    if (c) used.add(c.index);
    auto[t.name] = c ? c.index : null;
  }
  const assignment = input.assignment ?? auto;
  const maps: ColumnMap[] = targets.map((t) => {
    const idx = t.insertable ? (assignment[t.name] ?? null) : null;
    const c = idx === null ? undefined : profile.columns[idx];
    const m: ColumnMap = { target: t, file_index: idx, file_name: c?.name ?? null, conversion: { type: "text" }, issues: [], choices: [], suggested_type: null };
    if (!c) { if (t.insertable && !t.nullable) m.issues.push({ severity: "warning", message: "NOT NULL column has no value in the file: the insert fails unless the column has a DEFAULT or is filled by a trigger.", fix: null }); return m; }
    const text = t.data_type === "VARCHAR2";
    if (text) {
      if (t.char_length && c.max_chars > t.char_length) m.issues.push({ severity: "error", message: `The longest value has ${c.max_chars} characters but the column holds ${t.char_length}: those rows would be rejected.`, fix: `ALTER TABLE "${owner}"."${table}" MODIFY ("${t.name}" VARCHAR2(${Math.ceil(c.max_chars * 1.25)} CHAR))` });
    } else if (t.data_type === "NUMBER") {
      const dec = input.number_decimal?.[t.name] ?? c.number_style?.decimal ?? ".";
      m.conversion = { type: "number", decimal: dec };
      if (c.kind === "text" || c.kind === "date") {
        const bad = c.samples.find((s) => !num(s, dec)) ?? "";
        const hits = c.samples.filter((s) => num(s, dec)).length;
        m.issues.push(hits === 0 ? { severity: "error", message: `The file column holds text (e.g. '${bad}') but the target is ${t.display}.`, fix: null } : { severity: "warning", message: `Some values are not numbers (e.g. '${bad}'); those rows will be rejected.`, fix: null });
      } else if (t.precision !== null) {
        const room = t.precision - Math.max(t.scale ?? 0, 0);
        if (c.int_digits > room) m.issues.push({ severity: "error", message: `Values need ${c.int_digits} digits before the decimal point but NUMBER(${t.precision},${t.scale ?? 0}) allows ${room}.`, fix: `ALTER TABLE "${owner}"."${table}" MODIFY ("${t.name}" NUMBER(${c.int_digits + Math.max(t.scale ?? 0, 0)},${Math.max(t.scale ?? 0, 0)}))` });
      }
      if (c.number_ambiguous) {
        m.issues.push({ severity: "warning", message: `Numbers like ${c.samples[0]} could use '.' or ',' as the decimal mark; reading them with '${dec}'. Check this.`, fix: null });
        m.choices = [{ label: "1,234.56 (dot is the decimal mark)", value: "." }, { label: "1.234,56 (comma is the decimal mark)", value: "," }];
      }
    } else if (t.data_type === "DATE") {
      const fam = input.date_family?.[t.name] ?? c.date_formats[0];
      if (c.kind !== "date" || !fam) m.issues.push({ severity: "error", message: `No date format could be recognised in this column (e.g. '${c.samples[0] ?? ""}'); rows would be rejected.`, fix: null });
      else {
        m.conversion = { type: "date", family: fam };
        if (c.date_ambiguous) { m.issues.push({ severity: "warning", message: `Dates like ${c.samples[0]} could be read in more than one order; reading them as ${fam}. Check this.`, fix: null }); m.choices = c.date_formats.map((f) => ({ label: f, value: f })); }
      }
    }
    return m;
  });
  const all = maps.flatMap((m) => m.issues);
  return { owner, table, maps, unmapped_file: profile.columns.filter((c) => !maps.some((m) => m.file_index === c.index)).map((c) => c.name), by_position: false, errors: all.filter((i) => i.severity === "error").length, warnings: all.filter((i) => i.severity === "warning").length };
}

function convertAll(rows: string[][], plan: MappingPlan, max = Infinity): { ok: number; rejects: Reject[]; by: Map<string, number> } {
  const cols = plan.maps.filter((m) => m.file_index !== null && m.target.insertable);
  const by = new Map<string, number>(); const rejects: Reject[] = []; let ok = 0;
  rows.slice(0, max).forEach((r, i) => {
    for (const m of cols) {
      const v = (r[m.file_index!] ?? "").trim();
      let err: string | null = null;
      if (v === "") { if (!m.target.nullable) err = `required value missing for ${m.target.name}`; }
      else if (m.conversion.type === "number") { if (!num(v, m.conversion.decimal)) err = `'${v}' is not a valid number`; else { const c = canonNum(v, m.conversion.decimal).replace(/^[+-]/, "").split(".")[0].length; if (m.target.precision !== null && c > m.target.precision - Math.max(m.target.scale ?? 0, 0)) err = `${v} needs ${c} digits before the decimal point; ${m.target.display} allows ${m.target.precision - Math.max(m.target.scale ?? 0, 0)}`; } }
      else if (m.conversion.type === "date") { if (!parseDate(v, m.conversion.family)) err = `'${v}' is not a valid ${m.conversion.family} date`; }
      else if (m.target.data_type === "VARCHAR2" && m.target.char_length && v.length > m.target.char_length) err = `value too long for ${m.target.name} (${v.length} characters, limit ${m.target.char_length})`;
      if (err) { by.set(m.target.name, (by.get(m.target.name) ?? 0) + 1); rejects.push({ row: i + 1, reason: err, fields: r }); return; }
    }
    ok++;
  });
  return { ok, rejects, by };
}

export function runJob(kind: "export" | "import" | "validate" | "compare" | "sandbox", total: number, finish: (id: string) => unknown): string {
  const id = `job${++seq}`;
  (async () => {
    const t0 = performance.now();
    const steps = 8;
    for (let i = 1; i <= steps; i++) {
      await sleep(70);
      if (cancelled.has(id)) { emit({ type: "done", data: { id, kind, ok: false, cancelled: true, error: null, report: { cancelled: true, rows: Math.round((total * i) / steps) } } }); return; }
      emit({ type: "progress", data: { id, kind, rows: Math.round((total * i) / steps), bytes: Math.round((total * i * 40) / steps), rejected: 0, elapsed_ms: Math.round(performance.now() - t0) } });
    }
    try { emit({ type: "done", data: { id, kind, ok: true, cancelled: false, error: null, report: finish(id) } }); }
    catch (e) { emit({ type: "done", data: { id, kind, ok: false, cancelled: false, error: String(e), report: null } }); }
  })();
  return id;
}

function render(fmt: string, cols: string[], types: string[], rows: (string | null)[][], gz: boolean, header: boolean, delim: string, table: string | null): string {
  const q = (v: string | null, d: string) => (v === null ? "" : /[",\r\n]/.test(v) || v.includes(d) ? `"${v.replace(/"/g, '""')}"` : v);
  if (fmt === "json_lines") return rows.map((r) => JSON.stringify(Object.fromEntries(cols.map((c, i) => [c, r[i] !== null && /NUMBER/.test(types[i]) && /^-?\d{1,15}(\.\d+)?$/.test(r[i]!) ? Number(r[i]) : r[i]])))).join("\n") + "\n";
  if (fmt === "sql_insert") return `-- exported by Ora IDE\nSET DEFINE OFF;\n` + rows.map((r) => `INSERT INTO ${table ?? '"EXPORT_TABLE"'} (${cols.map((c) => `"${c}"`).join(", ")}) VALUES (${r.map((v, i) => (v === null ? "NULL" : /NUMBER/.test(types[i]) ? v : `'${v.replace(/'/g, "''")}'`)).join(", ")});`).join("\n") + "\nCOMMIT;\n";
  const d = fmt === "tsv" ? "\t" : delim;
  void gz;
  return (header ? cols.map((c) => q(c, d)).join(d) + "\r\n" : "") + rows.map((r) => r.map((v) => q(v, d)).join(d)).join("\r\n") + "\r\n";
}

export function transferApi(deps: TransferDeps): Pick<Api, "pickSavePath" | "pickOpenPath" | "onJob" | "jobCancel" | "exportRecommend" | "exportStart" | "importProfile" | "importPlan" | "importDdl" | "importCreateTable" | "importValidateStart" | "importStart"> {
  const rec = (rows: number | null, purpose: string): Recommendation => {
    const big = rows !== null && rows > 1_000_000;
    const fmt = purpose === "spreadsheet" ? (rows === null ? "csv" : rows <= 100_000 ? "xlsx" : "csv") : purpose === "load_oracle" && rows !== null && rows <= 10_000 ? "sql_insert" : "csv";
    const reasons = purpose === "spreadsheet" && rows !== null && rows > 1_048_576 ? [`${rows.toLocaleString()} rows exceed Excel's limit of 1,048,576 per sheet.`, "The data is split into files of 1,000,000 rows so each one opens in Excel."] : rows === null ? ["Row count unknown (ad-hoc query): streaming formats are the safe choice."] : [`${rows.toLocaleString()} rows: ${fmt === "xlsx" ? "fits comfortably in one Excel sheet." : "CSV streams in constant memory at any size."}`];
    const verdict = (f: string): "good" | "caution" | "avoid" => (f === "xlsx" ? (rows === null ? "caution" : rows > 1_048_576 ? "avoid" : rows > 100_000 ? "caution" : "good") : f === "sql_insert" ? (rows === null ? "caution" : rows > 200_000 ? "avoid" : rows > 10_000 ? "caution" : "good") : "good");
    return { format: fmt as Recommendation["format"], gzip: big, split_rows: purpose === "spreadsheet" && rows !== null && rows > 1_048_576 ? 1_000_000 : null, reasons, tip: purpose === "load_oracle" && rows !== null && rows > 5_000_000 ? "For tens of millions of rows between Oracle databases, Data Pump (expdp/impdp) runs inside the database and is much faster than any client-side export." : null,
      alternatives: (["csv", "xlsx", "sql_insert", "json_lines"] as const).map((f) => ({ format: f, verdict: verdict(f), note: f === "csv" ? "Streams in constant memory at any size and every tool reads it. Add gzip to cut the size 5-10x." : f === "xlsx" ? "Typed numbers and dates, header frozen, opens directly in Excel." : f === "sql_insert" ? "Self-contained and easy to review; fine at small sizes." : "One JSON object per line; streams like CSV and keeps types." })) };
  };
  return {
    pickSavePath: async (name) => `/demo/${name}`,
    pickOpenPath: (exts) => new Promise((resolve) => {
      const input = document.createElement("input");
      input.type = "file"; input.accept = exts.map((e) => `.${e}`).join(",");
      input.onchange = async () => { const f = input.files?.[0]; if (!f) return resolve(null); const p = `/demo/${f.name}`; files.set(p, await f.text()); resolve(p); };
      input.click();
    }),
    onJob: async (cb) => { listeners.add(cb); return () => void listeners.delete(cb); },
    jobCancel: async (id) => void cancelled.add(id),
    exportRecommend: async (conn, source, purpose) => {
      const d = deps.tableRows(conn, source);
      const est = source.type === "table" ? (d.rows.length < 5000 ? d.rows.length : 250_000) : null;
      await sleep(40);
      return { est_rows: est, from_statistics: est !== null, recommendation: rec(est, purpose) };
    },
    exportStart: async (conn, req) => {
      const d = deps.tableRows(conn, req.source);
      const rows = req.row_limit ? d.rows.slice(0, req.row_limit) : d.rows;
      const o = req.options;
      const ext = { csv: "csv", tsv: "tsv", json_lines: "jsonl", xlsx: "xlsx", sql_insert: "sql" }[o.format];
      const path = req.path.replace(/\.[a-z0-9]+$/i, "") + "." + ext + (o.gzip && o.format !== "xlsx" ? ".gz" : "");
      if (files.has(path) && !req.overwrite) throw `${path} already exists`;
      return runJob("export", rows.length, () => {
        const text = render(o.format, d.columns, d.types, rows, o.gzip, o.header, o.delimiter, req.source.type === "table" ? `"${req.source.owner}"."${req.source.table}"` : null);
        files.set(path, text);
        const report: ExportReport = { rows: rows.length, bytes: text.length, files: [path], elapsed_ms: 420, rows_per_sec: Math.round(rows.length / 0.42), warnings: o.format === "xlsx" ? ["Demo: the real app writes a true .xlsx workbook."] : [], cancelled: false };
        return report;
      });
    },
    importProfile: async (path) => {
      const text = files.get(path);
      if (text === undefined) throw `${path} is not a file`;
      await sleep(60);
      return { id: path, profile: profileOf(path, text) };
    },
    importPlan: async (conn, owner, table, id, input) => {
      const p = profiles.get(id); if (!p) throw "the file profile expired: choose the file again";
      return planOf(p.profile, deps.targets(conn, owner, table), owner, table, input);
    },
    importDdl: async (id, owner, table) => {
      const p = profiles.get(id)?.profile; if (!p) throw "the file profile expired";
      const ty = (c: ColProfile) => (c.kind === "integer" ? `NUMBER(${Math.max(c.int_digits, 1)})` : c.kind === "decimal" ? `NUMBER(${c.int_digits + c.scale},${c.scale})` : c.kind === "date" ? "DATE" : `VARCHAR2(${Math.max(16, Math.ceil(c.max_chars * 1.5))} CHAR)`);
      return `CREATE TABLE "${owner}"."${table}" (\n${p.columns.map((c) => `  "${c.name.toUpperCase().replace(/[^A-Z0-9_]/g, "_")}" ${ty(c)}`).join(",\n")}\n)`;
    },
    importCreateTable: async () => { await sleep(80); },
    importValidateStart: async (path, _r, plan) => {
      const p = profiles.get(path); if (!p) throw "the file profile expired";
      return runJob("validate", p.rows.length, () => { const r = convertAll(p.rows, plan); const rep: ValidationReport = { rows: p.rows.length, ok: r.ok, rejected: r.rejects.length, by_column: [...r.by.entries()].sort((a, b) => b[1] - a[1]), samples: r.rejects.slice(0, 50), cancelled: false }; return rep; });
    },
    importStart: async (_c, path, _r, plan, options) => {
      const p = profiles.get(path); if (!p) throw "the file profile expired";
      return runJob("import", p.rows.length, () => {
        const r = convertAll(p.rows, plan);
        const stopped = options.on_error === "stop" && r.rejects.length > 0 ? `stopped at row ${r.rejects[0].row}: ${r.rejects[0].reason}` : null;
        const rep: ImportReport = { read: p.rows.length, inserted: stopped ? 0 : r.ok, rejected: r.rejects.length, committed: !stopped, rolled_back: !!stopped && options.commit === "at_end", reject_file: r.rejects.length ? `${path}.rejects.csv` : null, stopped, cancelled: false, elapsed_ms: 480, rows_per_sec: Math.round(p.rows.length / 0.48), samples: r.rejects.slice(0, 50) };
        return rep;
      });
    },
  };
}
