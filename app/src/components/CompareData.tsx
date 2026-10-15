import { useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { errText } from "../hooks";
import type { Cell } from "./cell";
import type { DataDiffOptions, DataDiffReport, DataSide, SidePreview } from "../types";
import { JobBar, fmtMs, useJob } from "./Job";
import type { ConnState } from "./Explorer";

type Kind = "file" | "query" | "table";
interface SideState { kind: Kind; path: string; conn: string; sql: string; owner: string; table: string; decimal: string; preview?: SidePreview; error?: string }

const blank = (conns: ConnState[]): SideState => ({ kind: "file", path: "", conn: conns[0]?.id ?? "", sql: "", owner: conns[0]?.schemas[0] ?? "", table: "", decimal: "." });

function toSide(s: SideState): DataSide {
  return s.kind === "file" ? { type: "file", path: s.path, read: {} } : s.kind === "query" ? { type: "query", conn: s.conn, sql: s.sql } : { type: "table", conn: s.conn, owner: s.owner, table: s.table };
}
const ready = (s: SideState) => (s.kind === "file" ? !!s.path : s.kind === "query" ? !!s.sql.trim() : !!s.table.trim());

function SidePicker({ api, conns, title, value, onChange, onError }: { api: Api; conns: ConnState[]; title: string; value: SideState; onChange(s: SideState): void; onError(e: unknown): void }) {
  const [busy, setBusy] = useState(false);
  const conn = conns.find((c) => c.id === value.conn);
  const set = (p: Partial<SideState>) => onChange({ ...value, preview: undefined, error: undefined, ...p });
  const load = async (v: SideState = value) => {
    setBusy(true);
    try { onChange({ ...v, preview: await api.dataSidePreview(toSide(v)), error: undefined }); }
    catch (e) { onChange({ ...v, preview: undefined, error: errText(e) }); } finally { setBusy(false); }
  };
  const browse = async () => {
    try {
      const p = await api.pickOpenPath(["csv", "tsv", "txt", "gz", "xlsx", "xls"]);
      if (p) { const v = { ...value, path: p, preview: undefined, error: undefined }; onChange(v); await load(v); }
    } catch (e) { onError(e); }
  };
  return (
    <section className="cmp-src" aria-label={`Data ${title}`}>
      <header><b>{title}</b>
        <div className="segmented tight" role="radiogroup" aria-label={`${title} kind`}>
          {([["file", "File"], ["query", "Query"], ["table", "Table"]] as [Kind, string][]).map(([k, l]) => <button key={k} role="radio" aria-checked={value.kind === k} className={value.kind === k ? "on" : ""} onClick={() => set({ kind: k })}>{l}</button>)}
        </div>
      </header>
      {value.kind === "file" && <div className="opt-row grow"><button onClick={browse}>Browse…</button><span className="muted small mono grow">{value.path || "CSV, TSV, text, .gz or Excel"}</span></div>}
      {value.kind !== "file" && (
        <div className="opt-row">
          <label>Connection<select value={value.conn} onChange={(e) => set({ conn: e.target.value, owner: conns.find((c) => c.id === e.target.value)?.schemas[0] ?? "" })}>{conns.map((c) => <option key={c.id} value={c.id}>{c.profile.name}</option>)}</select></label>
          {value.kind === "table" && <>
            <label>Schema<select value={value.owner} onChange={(e) => set({ owner: e.target.value })}>{(conn?.schemas ?? []).map((s) => <option key={s}>{s}</option>)}</select></label>
            <label className="grow">Table<input value={value.table} onChange={(e) => set({ table: e.target.value.toUpperCase() })} aria-label={`${title} table`} placeholder="EMPLOYEES" /></label></>}
        </div>
      )}
      {value.kind === "query" && <textarea rows={3} spellCheck={false} aria-label={`${title} query`} placeholder="SELECT … (read-only; runs on its own session)" value={value.sql} onChange={(e) => set({ sql: e.target.value })} />}
      <div className="opt-row">
        {value.kind !== "file" && <button disabled={!ready(value) || busy} onClick={() => load()}>{busy ? "Reading…" : "Load columns"}</button>}
        <label>Decimal mark<select value={value.decimal} onChange={(e) => onChange({ ...value, decimal: e.target.value })} aria-label={`${title} decimal mark`}><option value=".">1234.5</option><option value=",">1234,5</option></select></label>
      </div>
      {value.error && <div className="error small" role="alert">{value.error}</div>}
      {value.preview && <div className="muted small" aria-label={`${title} columns`}>{value.preview.columns.length} columns: {value.preview.columns.slice(0, 12).join(", ")}{value.preview.columns.length > 12 ? "…" : ""}</div>}
    </section>
  );
}

const cellText = (c: Cell) => (c === null ? <i className="null">NULL</i> : c === "" ? <i className="null">(empty)</i> : c);

export function CompareData({ api, conns, onError }: { api: Api; conns: ConnState[]; onError(e: unknown): void }) {
  const [a, setA] = useState<SideState>(() => blank(conns));
  const [b, setB] = useState<SideState>(() => blank(conns));
  const [keys, setKeys] = useState<string[]>([]);
  const [o, setO] = useState({ trim: true, ignore_case: false, empty_is_null: true, numeric: true, dates: true });
  const [out, setOut] = useState("");
  const [tab, setTab] = useState<"changed" | "only_a" | "only_b">("changed");
  const job = useJob(api);

  const common = useMemo(() => {
    if (!a.preview || !b.preview) return [] as string[];
    const bn = new Set(b.preview.columns.map((c) => c.trim().toLowerCase()));
    return a.preview.columns.filter((c) => bn.has(c.trim().toLowerCase()));
  }, [a.preview, b.preview]);
  const suggestion = (a.preview?.suggested_keys ?? []).find((k) => k.every((c) => common.some((x) => x.toLowerCase() === c.toLowerCase())));
  useEffect(() => { setKeys((ks) => ks.filter((k) => common.includes(k))); }, [common]);

  const options = (): DataDiffOptions => ({ keys, ...o, a_decimal: a.decimal, b_decimal: b.decimal, max_samples: 200, memory_rows: 1_000_000, partitions: 256 });
  const run = () => job.start(() => api.dataDiffStart(toSide(a), toSide(b), options(), out || null));
  const pickOut = async () => { try { const p = await api.pickSavePath("differences.csv"); if (p) setOut(p); } catch (e) { onError(e); } };
  const rep = job.done?.ok ? (job.done.report as DataDiffReport) : undefined;
  const toggleKey = (c: string) => setKeys((ks) => (ks.includes(c) ? ks.filter((k) => k !== c) : [...ks, c]));
  const cancelled = job.done?.cancelled;

  return (
    <div className="cmp-pane">
      <div className="cmp-srcs">
        <SidePicker api={api} conns={conns} title="A" value={a} onChange={setA} onError={onError} />
        <SidePicker api={api} conns={conns} title="B" value={b} onChange={setB} onError={onError} />
      </div>
      <div className="cmp-keys">
        <div className="field-label">Key columns <span className="muted small">— rows are matched by these; leave empty to compare whole rows</span></div>
        {common.length === 0 ? <div className="muted small">Load both sides to choose key columns.</div> : (
          <div className="chips" role="group" aria-label="Key columns">
            {common.map((c) => <button key={c} className={`chipbtn ${keys.includes(c) ? "on" : ""}`} aria-pressed={keys.includes(c)} onClick={() => toggleKey(c)}>{c}</button>)}
            {suggestion && keys.length === 0 && <button className="ghost" onClick={() => setKeys(suggestion.map((s) => common.find((x) => x.toLowerCase() === s.toLowerCase())!))}>Use suggested: {suggestion.join(" + ")}</button>}
          </div>
        )}
      </div>
      <div className="opt-row cmp-opts">
        <label className="check" title="Ignore spaces around values"><input type="checkbox" checked={o.trim} onChange={(e) => setO({ ...o, trim: e.target.checked })} /> Trim spaces</label>
        <label className="check"><input type="checkbox" checked={o.ignore_case} onChange={(e) => setO({ ...o, ignore_case: e.target.checked })} /> Ignore case</label>
        <label className="check" title="A CSV cannot tell an empty cell from NULL, and Oracle treats '' as NULL"><input type="checkbox" checked={o.empty_is_null} onChange={(e) => setO({ ...o, empty_is_null: e.target.checked })} /> Empty = NULL</label>
        <label className="check" title="1, 1.0 and +1 are the same number"><input type="checkbox" checked={o.numeric} onChange={(e) => setO({ ...o, numeric: e.target.checked })} /> Numbers by value</label>
        <label className="check" title="2021-05-01 equals 2021-05-01 00:00:00"><input type="checkbox" checked={o.dates} onChange={(e) => setO({ ...o, dates: e.target.checked })} /> Dates by value</label>
        <span className="spacer" />
        <button onClick={pickOut} title="Optional: every difference, one cell per line">{out ? "Differences file ✓" : "Save differences…"}</button>
        <button className="primary" disabled={!ready(a) || !ready(b) || job.running} onClick={run}>Compare</button>
      </div>
      {out && <div className="muted small mono">Differences will be written to {out}</div>}
      {job.startError && <div className="error" role="alert">{job.startError}</div>}
      {job.running && <JobBar progress={job.progress} label="Comparing" onCancel={() => job.id && api.jobCancel(job.id).catch(onError)} />}
      {job.done && !job.done.ok && !cancelled && <div className="error" role="alert">{job.done.error}</div>}
      {cancelled && <div className="warn-box">Cancelled: the numbers below, if any, are incomplete.</div>}
      {rep && (
        <div className="cmp-result data" aria-label="Data comparison result">
          <div className={`cmp-stats ${rep.identical ? "same" : ""}`} role="status">
            {rep.identical ? <b>Identical: {rep.rows_a.toLocaleString()} rows match.</b> : <b>The two sides differ.</b>}
            <span className="muted small"> {rep.rows_a.toLocaleString()} rows in A, {rep.rows_b.toLocaleString()} in B · {fmtMs(rep.elapsed_ms)}{rep.spilled ? " · large: compared via disk" : ""}{rep.whole_row ? " · whole rows" : ` · key ${rep.key_columns.join(" + ")}`}</span>
          </div>
          <div className="cards">
            <div className="card ok"><b>{rep.same.toLocaleString()}</b><span>same</span></div>
            <div className="card warn"><b>{rep.changed.toLocaleString()}</b><span>changed</span></div>
            <div className="card bad"><b>{rep.only_a.toLocaleString()}</b><span>only in A</span></div>
            <div className="card bad"><b>{rep.only_b.toLocaleString()}</b><span>only in B</span></div>
          </div>
          {rep.warnings.map((w, i) => <div key={i} className="warn-box small">{w}</div>)}
          {rep.diff_file && <div className="ok-box small">Every difference is in <span className="mono">{rep.diff_file}</span> (one cell per line).</div>}
          {rep.by_column.length > 0 && (
            <div className="bycol" aria-label="Differences by column">
              <div className="field-label">Where the changes are</div>
              {rep.by_column.slice(0, 12).map(([c, n]) => <div key={c} className="bar"><span>{c}</span><div><i style={{ width: `${Math.max(3, (n / rep.by_column[0][1]) * 100)}%` }} /></div><b>{n.toLocaleString()}</b></div>)}
            </div>
          )}
          {!rep.identical && (
            <>
              <div className="segmented tight" role="tablist" aria-label="Samples">
                {([["changed", `Changed (${rep.changed.toLocaleString()})`], ["only_a", `Only in A (${rep.only_a.toLocaleString()})`], ["only_b", `Only in B (${rep.only_b.toLocaleString()})`]] as ["changed" | "only_a" | "only_b", string][]).map(([k, l]) => <button key={k} role="tab" aria-selected={tab === k} className={tab === k ? "on" : ""} onClick={() => setTab(k)}>{l}</button>)}
              </div>
              <div className="map-wrap">
                {tab === "changed" ? (
                  <table className="map" aria-label="Changed rows">
                    <thead><tr>{rep.key_columns.map((k) => <th key={k}>{k}</th>)}<th>Column</th><th>A</th><th>B</th></tr></thead>
                    <tbody>{rep.changed_samples.flatMap((r, i) => r.diffs.map((d, j) => <tr key={`${i}-${j}`}>{rep.key_columns.map((k, x) => <td key={k}>{j === 0 ? cellText(r.key[x]) : ""}</td>)}<td><b>{d.column}</b></td><td className="d-del">{cellText(d.a)}</td><td className="d-ins">{cellText(d.b)}</td></tr>))}</tbody>
                  </table>
                ) : (
                  <table className="map" aria-label={tab === "only_a" ? "Rows only in A" : "Rows only in B"}>
                    <thead><tr>{rep.layout.map((c) => <th key={c}>{c}</th>)}</tr></thead>
                    <tbody>{(tab === "only_a" ? rep.only_a_samples : rep.only_b_samples).map((r, i) => <tr key={i}>{r.map((c, j) => <td key={j}>{cellText(c)}</td>)}</tr>)}</tbody>
                  </table>
                )}
              </div>
              <div className="muted small">Showing up to 200 samples per list{rep.diff_file ? "; the differences file has them all" : "; save a differences file to get them all"}.</div>
            </>
          )}
        </div>
      )}
    </div>
  );
}
