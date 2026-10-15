import { useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { useAsync } from "../hooks";
import type { Environment, FileProfile, ImportOptions, ImportReport, MappingPlan, PlanInput, ReadOptions, ValidationReport } from "../types";
import { JobBar, fmtMs, useJob } from "./Job";

const DELIMS: [string, string][] = [["", "auto"], [",", "comma"], [";", "semicolon"], ["\t", "tab"], ["|", "pipe"]];
const defaults: ImportOptions = { batch_size: 5000, commit: "at_end", on_error: "skip", max_rejects: 10000, truncate_first: false, trim: false, append_hint: false };

export function ImportWizard({ api, conn, env, schemas, initial, onClose, onError, onImported }: {
  api: Api; conn: string; env: Environment; schemas: string[]; initial?: { owner: string; table: string };
  onClose(): void; onError(e: unknown): void; onImported?(owner: string, table: string): void;
}) {
  const [path, setPath] = useState("");
  const [read, setRead] = useState<ReadOptions>({});
  const [prof, setProf] = useState<{ id: string; profile: FileProfile }>();
  const [profErr, setProfErr] = useState("");
  const [owner, setOwner] = useState(initial?.owner ?? schemas[0] ?? "");
  const [table, setTable] = useState(initial?.table ?? "");
  const [input, setInput] = useState<PlanInput>({});
  const [plan, setPlan] = useState<MappingPlan>();
  const [planErr, setPlanErr] = useState("");
  const [ddl, setDdl] = useState("");
  const [opts, setOpts] = useState<ImportOptions>(defaults);
  const [ack, setAck] = useState(false);
  const [validation, setValidation] = useState<ValidationReport>();
  const [result, setResult] = useState<ImportReport>();
  const job = useJob(api);
  const tables = useAsync(() => api.listObjects(conn, owner, "TABLE"), [conn, owner]);

  const loadFile = async (p: string, r: ReadOptions) => {
    setProfErr(""); setPlan(undefined); setValidation(undefined); setInput({});
    try { setProf(await api.importProfile(p, r)); } catch (e) { setProf(undefined); setProfErr(String(e instanceof Error ? e.message : e)); }
  };
  const browse = async () => {
    try {
      const p = await api.pickOpenPath(["csv", "tsv", "txt", "gz", "xlsx", "xls"]);
      if (!p) return;
      setPath(p); setRead({}); await loadFile(p, {});
      if (!table) setTable(p.split(/[\\/]/).pop()!.replace(/\.(csv|tsv|txt|xlsx?)(\.gz)?$/i, "").toUpperCase().replace(/[^A-Z0-9_]+/g, "_"));
    } catch (e) { onError(e); }
  };
  const reread = (p: Partial<ReadOptions>) => { const r = { ...read, ...p }; setRead(r); void loadFile(path, r); };

  const inputKey = JSON.stringify(input);
  useEffect(() => {
    if (!prof || !owner || !table.trim()) { setPlan(undefined); return; }
    let live = true;
    const t = setTimeout(() => {
      api.importPlan(conn, owner, table.trim(), prof.id, input).then(
        (p) => { if (live) { setPlan(p); setPlanErr(""); } },
        (e) => { if (live) { setPlan(undefined); setPlanErr(String(e instanceof Error ? e.message : e)); } },
      );
    }, 250);
    return () => { live = false; clearTimeout(t); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, conn, owner, table, prof, inputKey]);
  useEffect(() => { setValidation(undefined); }, [plan, opts.trim]);

  const done = job.done;
  useEffect(() => {
    if (!done?.ok) return;
    if (done.kind === "validate") setValidation(done.report as ValidationReport);
    if (done.kind === "import") { setResult(done.report as ImportReport); onImported?.(owner, table.trim()); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [done]);

  const fileCols = prof?.profile.columns ?? [];
  const setAssign = (target: string, idx: number | null) => {
    const cur = Object.fromEntries((plan?.maps ?? []).map((m) => [m.target.name, m.file_index]));
    setInput({ ...input, assignment: { ...cur, ...(input.assignment ?? {}), [target]: idx } });
  };
  const setChoice = (target: string, kind: "date" | "number", v: string) =>
    setInput(kind === "date" ? { ...input, date_family: { ...input.date_family, [target]: v } } : { ...input, number_decimal: { ...input.number_decimal, [target]: v } });

  const needsCreate = !!prof && !!table.trim() && !plan && !!planErr;
  const showDdl = async () => {
    try { setDdl(await api.importDdl(prof!.id, owner, table.trim().toUpperCase())); } catch (e) { onError(e); }
  };
  const createTable = async () => {
    try { await api.importCreateTable(conn, ddl); setDdl(""); setTable((t) => t.trim().toUpperCase()); setInput({ ...input }); } catch (e) { onError(e); }
  };

  const ready = !!plan && plan.errors === 0 && (env !== "prod" || ack);
  const running = job.running;
  const showForm = !running && !result;
  const issues = useMemo(() => plan?.maps.reduce((n, m) => n + m.issues.length, 0) ?? 0, [plan]);
  const dis = (p: Partial<ImportOptions>) => setOpts((o) => ({ ...o, ...p }));

  return (
    <div className="modal-back">
      <div className="modal wide import" role="dialog" aria-label="Import data">
        <h3>Import data</h3>
        {showForm && (
          <>
            <div className="opt-row grow">
              <label className="grow">File<input value={path} readOnly placeholder="CSV, TSV, text or Excel file…" aria-label="Input file" /></label>
              <button onClick={browse}>Browse…</button>
            </div>
            {profErr && <div className="error" role="alert">{profErr}</div>}
            {prof && (
              <div className="file-info" aria-label="File summary">
                <span>{prof.profile.format.toUpperCase()}</span><span>{prof.profile.encoding}</span>
                <span>{prof.profile.rows_scanned.toLocaleString()}{prof.profile.more_rows ? "+" : ""} rows scanned</span>
                <span>{fileCols.length} columns</span>
                {prof.profile.ragged_rows > 0 && <span className="warn">{prof.profile.ragged_rows} rows with a different column count</span>}
                {prof.profile.delimiter !== null && (
                  <label className="inline">Delimiter
                    <select value={read.delimiter ?? ""} onChange={(e) => reread({ delimiter: e.target.value || null })}>{DELIMS.map(([v, n]) => <option key={n} value={v}>{n}</option>)}</select></label>
                )}
                <label className="inline">Header
                  <select value={read.has_header === undefined || read.has_header === null ? "" : String(read.has_header)} onChange={(e) => reread({ has_header: e.target.value === "" ? null : e.target.value === "true" })}>
                    <option value="">auto ({prof.profile.has_header ? "yes" : "no"})</option><option value="true">first row is header</option><option value="false">no header</option></select></label>
                {prof.profile.sheets.length > 1 && (
                  <label className="inline">Sheet<select value={read.sheet ?? prof.profile.sheets[0]} onChange={(e) => reread({ sheet: e.target.value })}>{prof.profile.sheets.map((s) => <option key={s}>{s}</option>)}</select></label>
                )}
              </div>
            )}
            {prof?.profile.warnings.map((w, i) => <div key={i} className="warn-box">{w}</div>)}

            <div className="opt-row">
              <label>Schema<select value={owner} onChange={(e) => { setOwner(e.target.value); setInput({}); }}>{schemas.map((s) => <option key={s}>{s}</option>)}</select></label>
              <label className="grow">Table<input list="imp-tables" value={table} onChange={(e) => { setTable(e.target.value); setInput({}); }} placeholder="existing table, or a new name" aria-label="Target table" /></label>
              <datalist id="imp-tables">{(tables.data ?? []).map((t) => <option key={t.name} value={t.name} />)}</datalist>
            </div>

            {needsCreate && !ddl && (
              <div className="warn-box">
                {planErr}
                <div className="actions left"><button onClick={showDdl}>Create table from this file…</button></div>
              </div>
            )}
            {ddl && (
              <div className="ddl-box">
                <div className="field-label">This will run on {env === "prod" ? "PRODUCTION" : "the connected database"} (DDL commits immediately):</div>
                <pre className="sqlpreview" aria-label="Create table statement">{ddl}</pre>
                <div className="actions"><button onClick={() => setDdl("")}>Cancel</button><button className={env === "prod" ? "danger" : "primary"} onClick={createTable}>Create table</button></div>
              </div>
            )}

            {plan && (
              <>
                <div className="field-label">Column mapping
                  <span className="muted small"> {plan.by_position ? "matched by position (no header names)" : "matched by name"}{plan.errors > 0 ? ` · ${plan.errors} problem${plan.errors > 1 ? "s" : ""} to fix` : issues > 0 ? ` · ${plan.warnings} warning${plan.warnings === 1 ? "" : "s"}` : " · all good"}</span></div>
                <div className="map-wrap">
                  <table className="map" aria-label="Column mapping">
                    <thead><tr><th>Table column</th><th>Type</th><th>File column</th><th>Reads as</th><th>Notes</th></tr></thead>
                    <tbody>
                      {plan.maps.map((m) => (
                        <tr key={m.target.name} className={m.issues.some((i) => i.severity === "error") ? "bad" : ""}>
                          <td><b>{m.target.name}</b>{!m.target.nullable && <span className="muted small"> NOT NULL</span>}</td>
                          <td className="mono small">{m.target.display}</td>
                          <td>
                            {m.target.insertable ? (
                              <select aria-label={`File column for ${m.target.name}`} value={m.file_index ?? ""} onChange={(e) => setAssign(m.target.name, e.target.value === "" ? null : Number(e.target.value))}>
                                <option value="">— skip (NULL/default) —</option>
                                {fileCols.map((c) => <option key={c.index} value={c.index}>{c.name}</option>)}
                              </select>
                            ) : <span className="muted small">{m.target.note ?? "not loadable"}</span>}
                          </td>
                          <td className="small">
                            {m.file_index !== null && (m.choices.length > 1 ? (
                              <select aria-label={`Format for ${m.target.name}`} value={m.conversion.type === "date" || m.conversion.type === "timestamp" ? m.conversion.family : m.conversion.type === "number" ? m.conversion.decimal : ""}
                                onChange={(e) => setChoice(m.target.name, m.conversion.type === "number" ? "number" : "date", e.target.value)}>
                                {m.choices.map((c) => <option key={c.value} value={c.value}>{c.label}</option>)}
                              </select>
                            ) : <span className="muted">{m.conversion.type}</span>)}
                          </td>
                          <td className="small">
                            {m.issues.map((i, k) => <div key={k} className={`issue ${i.severity}`}>{i.message}{i.fix && <code>{i.fix}</code>}</div>)}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {plan.unmapped_file.length > 0 && <div className="muted small">Not loaded (no matching table column): {plan.unmapped_file.join(", ")}</div>}

                <div className="opt-row">
                  <label>Commit<select value={opts.commit} onChange={(e) => dis({ commit: e.target.value as ImportOptions["commit"] })}>
                    <option value="at_end">once at the end (all or nothing)</option><option value="per_batch">after every batch (keeps progress)</option></select></label>
                  <label>Bad rows<select value={opts.on_error} onChange={(e) => dis({ on_error: e.target.value as ImportOptions["on_error"] })}>
                    <option value="skip">skip and write to a reject file</option><option value="stop">stop at the first one</option></select></label>
                  <label>Batch size<input type="number" min={100} step={500} value={opts.batch_size} onChange={(e) => dis({ batch_size: Math.max(100, Number(e.target.value)) })} /></label>
                </div>
                <div className="opt-row">
                  <label className="check"><input type="checkbox" checked={opts.trim} onChange={(e) => dis({ trim: e.target.checked })} /> Trim spaces around values</label>
                  <label className="check"><input type="checkbox" checked={opts.truncate_first} onChange={(e) => dis({ truncate_first: e.target.checked })} /> Empty the table first (TRUNCATE)</label>
                  <label className="check" title="Faster for big loads; the table is locked while loading"><input type="checkbox" checked={opts.append_hint} onChange={(e) => dis({ append_hint: e.target.checked })} /> Direct-path (APPEND)</label>
                </div>
                {opts.truncate_first && <div className="warn-box">TRUNCATE cannot be rolled back. Existing rows in {owner}.{table.trim()} are deleted before loading.</div>}
                {env === "prod" && <label className="check danger-text"><input type="checkbox" checked={ack} onChange={(e) => setAck(e.target.checked)} /> I understand this writes to a PRODUCTION database</label>}

                {validation && (
                  <div className={validation.rejected ? "warn-box" : "ok-box"} role="status" aria-label="Validation result">
                    Checked {validation.rows.toLocaleString()} rows without touching the database: <b>{validation.ok.toLocaleString()} would load</b>
                    {validation.rejected > 0 && <>, <b>{validation.rejected.toLocaleString()} would be rejected</b></>}.
                    {validation.by_column.length > 0 && <div className="small">By column: {validation.by_column.map(([c, n]) => `${c} (${n})`).join(", ")}</div>}
                    {validation.samples.slice(0, 5).map((s) => <div key={s.row} className="small mono">row {s.row}: {s.reason}</div>)}
                  </div>
                )}
              </>
            )}
            {job.startError && <div className="error" role="alert">{job.startError}</div>}
            {done && !done.ok && <div className="error" role="alert">{done.cancelled ? "Cancelled." : done.error}</div>}
            <div className="actions">
              <button onClick={onClose}>Close</button>
              <button disabled={!ready} title="Reads the whole file and checks every value; nothing is written" onClick={() => job.start(() => api.importValidateStart(path, read, plan!, opts.trim))}>Check file</button>
              <button className={env === "prod" ? "danger" : "primary"} disabled={!ready} onClick={() => job.start(() => api.importStart(conn, path, read, plan!, opts))}>Import</button>
            </div>
          </>
        )}

        {running && <JobBar progress={job.progress} label={done?.kind === "validate" || job.progress?.kind === "validate" ? "Checking" : "Loading"} onCancel={() => job.id && api.jobCancel(job.id).catch(onError)} />}

        {result && (
          <div className="job-result">
            <div className={result.stopped || result.cancelled || result.rolled_back ? "warn-box" : "ok-box"}>
              {result.cancelled ? "Cancelled." : result.stopped ? `Stopped: ${result.stopped}` : "Import finished."}
              {result.rolled_back ? " Nothing was kept (rolled back)." : result.committed ? " Committed." : " Not committed."}
            </div>
            <dl className="xkv">
              <dt>Read</dt><dd>{result.read.toLocaleString()}</dd>
              <dt>Inserted</dt><dd>{result.inserted.toLocaleString()}</dd>
              <dt>Rejected</dt><dd>{result.rejected.toLocaleString()}</dd>
              <dt>Time</dt><dd>{fmtMs(result.elapsed_ms)} ({result.rows_per_sec.toLocaleString()} rows/s)</dd>
              {result.reject_file && <><dt>Reject file</dt><dd className="mono">{result.reject_file}</dd></>}
            </dl>
            {result.samples.slice(0, 5).map((s) => <div key={s.row} className="small mono">row {s.row}: {s.reason}</div>)}
            <div className="actions"><button className="primary" onClick={onClose}>Done</button></div>
          </div>
        )}
      </div>
    </div>
  );
}
