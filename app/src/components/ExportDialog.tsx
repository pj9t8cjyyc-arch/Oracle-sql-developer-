import { useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { errText, useAsync } from "../hooks";
import { fmtBytes } from "../format";
import type { Environment, ExportFormat, ExportOptions, ExportReport, ExportSource, Purpose } from "../types";
import { JobBar, fmtMs, useJob } from "./Job";

const PURPOSES: [Purpose, string, string][] = [
  ["spreadsheet", "Open in Excel", "People will read or filter it"],
  ["load_oracle", "Load into Oracle", "Another Oracle table or database"],
  ["exchange", "Share with a tool", "Python, BI, other databases"],
  ["archive", "Archive / backup", "Smallest, keep for later"],
];
const FORMATS: [ExportFormat, string, string][] = [
  ["csv", "CSV", ".csv"], ["xlsx", "Excel", ".xlsx"], ["sql_insert", "SQL INSERT", ".sql"], ["json_lines", "JSON Lines", ".jsonl"], ["tsv", "TSV", ".tsv"],
];
const EXT: Record<ExportFormat, string> = { csv: "csv", tsv: "tsv", json_lines: "jsonl", xlsx: "xlsx", sql_insert: "sql" };
const VERDICT: Record<string, string> = { good: "Good fit", caution: "Works, with caveats", avoid: "Not advised" };

const base: ExportOptions = {
  format: "csv", header: true, delimiter: ",", null_text: "", bom: true, gzip: false, sanitize_formulas: true, split_rows: null,
  insert_table: null, rows_per_commit: 1000, sqlplus_directives: true, xlsx_sheet_rows: 1_048_576, batch_size: 5000,
};

export function ExportDialog({ api, conn, env, source, label, defaultName, onClose, onError }: {
  api: Api; conn: string; env: Environment; source: ExportSource; label: string; defaultName: string; onClose(): void; onError(e: unknown): void;
}) {
  const [purpose, setPurpose] = useState<Purpose>("spreadsheet");
  const [opts, setOpts] = useState<ExportOptions>({ ...base, insert_table: source.type === "table" ? `${source.owner}.${source.table}` : null });
  const [touched, setTouched] = useState(false);
  const [path, setPath] = useState("");
  const [limit, setLimit] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [shown, setShown] = useState<ExportReport>();
  const job = useJob(api);
  const rec = useAsync(() => api.exportRecommend(conn, source, purpose), [conn, purpose, JSON.stringify(source)]);
  const r = rec.data?.recommendation;

  // Follow the recommendation until the person picks something themselves.
  useEffect(() => {
    if (!r || touched) return;
    setOpts((o) => ({ ...o, format: r.format, gzip: r.gzip, split_rows: r.split_rows }));
  }, [r, touched]);
  const set = (p: Partial<ExportOptions>) => { setTouched(true); setOpts((o) => ({ ...o, ...p })); };
  const verdict = useMemo(() => {
    const m = new Map(r?.alternatives.map((a) => [a.format, a]));
    if (r) m.set(r.format, { format: r.format, verdict: "good", note: r.reasons[0] ?? "Recommended" });
    return m;
  }, [r]);

  
  const pick = async () => {
    try { const p = await api.pickSavePath(`${defaultName}.${EXT[opts.format]}${opts.gzip ? ".gz" : ""}`); if (p) setPath(p); } catch (e) { onError(e); }
  };
  const start = () => job.start(() => api.exportStart(conn, {
    source, path, options: { ...opts, delimiter: opts.format === "tsv" ? "\t" : opts.delimiter }, overwrite,
    row_limit: limit.trim() ? Math.max(1, Math.floor(Number(limit))) : null,
  }));
  const done = job.done;
  const report = done?.ok ? (done.report as ExportReport) : undefined;
  useEffect(() => { if (report) setShown(report); }, [report]);

  const busy = job.running;
  const est = rec.data?.est_rows;
  return (
    <div className="modal-back">
      <div className="modal wide export" role="dialog" aria-label="Export data">
        <h3>Export <span className="muted">{label}</span></h3>
        {env === "prod" && <div className="warn-box">Production database — this only reads data, but check the destination before you share the file.</div>}

        {!busy && !done && (
          <>
            <div className="field-label">What is it for?</div>
            <div className="chips" role="radiogroup" aria-label="Purpose">
              {PURPOSES.map(([id, t, d]) => (
                <button key={id} role="radio" aria-checked={purpose === id} className={`chipbtn ${purpose === id ? "on" : ""}`} title={d}
                  onClick={() => { setPurpose(id); setTouched(false); }}>{t}</button>
              ))}
            </div>

            <div className="rec" aria-live="polite">
              {rec.loading && <span className="muted">Estimating size…</span>}
              {rec.error && <span className="muted">Could not estimate size ({rec.error}). Pick a format below.</span>}
              {r && (
                <>
                  <div className="rec-head">
                    <b>Suggested: {FORMATS.find((f) => f[0] === r.format)?.[1]}{r.gzip ? " + gzip" : ""}{r.split_rows ? `, ${r.split_rows.toLocaleString()} rows per file` : ""}</b>
                    <span className="muted">{est === null || est === undefined ? "row count unknown" : `${rec.data?.from_statistics ? "≈ " : ""}${est.toLocaleString()} rows`}</span>
                  </div>
                  <ul>{r.reasons.map((x, i) => <li key={i}>{x}</li>)}</ul>
                  {r.tip && <div className="tip">Tip: {r.tip}</div>}
                </>
              )}
            </div>

            <div className="fmt-grid" role="radiogroup" aria-label="Format">
              {FORMATS.map(([id, name, ext]) => {
                const v = verdict.get(id);
                return (
                  <button key={id} role="radio" aria-checked={opts.format === id} className={`fmt ${opts.format === id ? "on" : ""} v-${v?.verdict ?? "none"}`} onClick={() => set({ format: id })}>
                    <b>{name}</b><span className="muted">{ext}</span>
                    {v && <span className={`verdict ${v.verdict}`}>{VERDICT[v.verdict]}</span>}
                    {v && <span className="small muted">{v.note}</span>}
                  </button>
                );
              })}
            </div>

            <div className="opt-row">
              {(opts.format === "csv") && <label>Delimiter
                <select value={opts.delimiter} onChange={(e) => set({ delimiter: e.target.value })}>
                  <option value=",">comma ,</option><option value=";">semicolon ;</option><option value="|">pipe |</option>
                </select></label>}
              {opts.format !== "json_lines" && opts.format !== "sql_insert" && <label className="check"><input type="checkbox" checked={opts.header} onChange={(e) => set({ header: e.target.checked })} /> Header row</label>}
              {opts.format === "csv" && <label className="check" title="Excel needs this to read accents correctly"><input type="checkbox" checked={opts.bom} onChange={(e) => set({ bom: e.target.checked })} /> UTF-8 BOM (for Excel)</label>}
              {opts.format !== "xlsx" && <label className="check"><input type="checkbox" checked={opts.gzip} onChange={(e) => set({ gzip: e.target.checked })} /> Compress (.gz)</label>}
              {(opts.format === "csv" || opts.format === "tsv" || opts.format === "xlsx") && <label className="check" title="Prefixes text cells that start with = + - @ so Excel does not run them as formulas"><input type="checkbox" checked={opts.sanitize_formulas} onChange={(e) => set({ sanitize_formulas: e.target.checked })} /> Neutralise formulas</label>}
            </div>
            {opts.format === "sql_insert" && (
              <div className="opt-row">
                <label>Target table<input value={opts.insert_table ?? ""} onChange={(e) => set({ insert_table: e.target.value })} placeholder="SCHEMA.TABLE" /></label>
                <label>Rows per COMMIT<input type="number" min={0} value={opts.rows_per_commit} onChange={(e) => set({ rows_per_commit: Number(e.target.value) })} /></label>
              </div>
            )}
            <div className="opt-row">
              <label>Split into files of<input type="number" min={0} placeholder="no split" value={opts.split_rows ?? ""} onChange={(e) => set({ split_rows: e.target.value ? Number(e.target.value) : null })} /> <span className="small muted">rows</span></label>
              <label>Only the first<input type="number" min={1} placeholder="all" value={limit} onChange={(e) => setLimit(e.target.value)} /> <span className="small muted">rows</span></label>
            </div>
            <div className="opt-row grow">
              <label className="grow">Save to<input value={path} onChange={(e) => setPath(e.target.value)} placeholder="Choose a file…" aria-label="Output file" /></label>
              <button onClick={pick}>Browse…</button>
            </div>
            <label className="check"><input type="checkbox" checked={overwrite} onChange={(e) => setOverwrite(e.target.checked)} /> Replace the file if it exists</label>
            {job.startError && <div className="error" role="alert">{job.startError}</div>}
            <div className="actions">
              <button onClick={onClose}>Close</button>
              <button className="primary" disabled={!path.trim() || (opts.format === "sql_insert" && !opts.insert_table?.trim())} onClick={start}>Export</button>
            </div>
          </>
        )}

        {busy && <JobBar progress={job.progress} label="Writing" onCancel={() => job.id && api.jobCancel(job.id).catch(onError)} />}

        {done && (
          <div className="job-result">
            {done.ok && shown ? (
              <>
                <div className={shown.cancelled ? "warn-box" : "ok-box"}>{shown.cancelled ? "Cancelled — the partial file was removed." : "Export finished."}</div>
                <dl className="xkv">
                  <dt>Rows</dt><dd>{shown.rows.toLocaleString()}</dd>
                  <dt>Size</dt><dd>{fmtBytes(shown.bytes)}</dd>
                  <dt>Time</dt><dd>{fmtMs(shown.elapsed_ms)} ({shown.rows_per_sec.toLocaleString()} rows/s)</dd>
                  <dt>Files</dt><dd>{shown.files.map((f) => <div key={f} className="mono">{f}</div>)}</dd>
                </dl>
                {shown.warnings.map((w, i) => <div key={i} className="warn-box">{w}</div>)}
              </>
            ) : <div className="error" role="alert">{done.cancelled ? "Cancelled." : done.error ?? errText("Export failed")}</div>}
            <div className="actions">
              <button onClick={() => { job.reset(); setShown(undefined); }}>Export again</button>
              <button className="primary" onClick={onClose}>Done</button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
