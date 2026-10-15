import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Api } from "../api";
import { errText, useAsync } from "../hooks";
import { fmtBytes, timeAgo } from "../format";
import type { CloneSpec, ClonePlan, Column, SandboxInfo, SandboxMeta, SandboxRun, StatementRange, TableBrief } from "../types";
import type { ConnState } from "./Explorer";
import { JobBar, useJob } from "./Job";
import { ResultGrid } from "./ResultGrid";
import { SqlEditor, type EditorHandle } from "./SqlEditor";

const noComplete = async () => ({ from: 0, items: [] });
const today = () => new Date().toISOString().slice(0, 10).replace(/-/g, "");
const asColumns = (names: string[]): Column[] => names.map((name) => ({ name, data_type: "", nullable: true }));

/** A local SQLite copy of chosen tables (or a whole schema) to try queries and changes on. Nothing here touches Oracle. */
export function SandboxPage({ api, conns, preset, onError }: { api: Api; conns: ConnState[]; preset?: { sql: string; nonce: number }; onError(e: unknown): void }) {
  const list = useAsync(() => api.sandboxList(), [api]);
  const [sel, setSel] = useState<string | "new">();
  const boxes = list.data ?? [];
  useEffect(() => { if (sel === undefined && list.data) setSel(list.data[0]?.meta.name ?? "new"); }, [list.data, sel]);
  const current = boxes.find((b) => b.meta.name === sel);
  return (
    <div className="sb-page">
      <aside className="sb-side">
        <div className="sb-side-head"><b>Sandboxes</b><span className="grow" /><button className="primary" onClick={() => setSel("new")}>+ New</button></div>
        {boxes.length === 0 && <div className="muted pad small">A sandbox is a private copy of some tables on this computer. Run, change and break things there; Oracle is never touched.</div>}
        {boxes.map((b) => (
          <button key={b.meta.name} className={`sb-item ${sel === b.meta.name ? "on" : ""}`} onClick={() => setSel(b.meta.name)}>
            <b>{b.meta.name}</b>{b.modified && <span className="sb-mod" title="Changed since it was cloned">changed</span>}
            <span className="muted small">{b.meta.source} · {b.meta.owner} · {b.meta.tables.length} tables · {fmtBytes(b.bytes)}</span>
          </button>
        ))}
      </aside>
      <section className="sb-main">
        {sel === "new" || (sel === undefined) ? <NewSandbox api={api} conns={conns} onDone={(name) => { list.reload(); setSel(name); }} onError={onError} />
          : current ? <SandboxView key={current.meta.name} api={api} info={current} preset={preset} onChanged={list.reload} onDeleted={() => { setSel(undefined); list.reload(); }} onError={onError} />
          : <div className="muted pad">Choose a sandbox.</div>}
      </section>
    </div>
  );
}

function NewSandbox({ api, conns, onDone, onError }: { api: Api; conns: ConnState[]; onDone(name: string): void; onError(e: unknown): void }) {
  const [connId, setConnId] = useState(conns[0]?.id ?? "");
  const conn = conns.find((c) => c.id === connId);
  const [owner, setOwner] = useState(conn?.schemas[0] ?? "");
  const [name, setName] = useState("");
  const [all, setAll] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");
  const [limitOn, setLimitOn] = useState(false);
  const [limit, setLimit] = useState(1000);
  const [parents, setParents] = useState(true);
  const [indexes, setIndexes] = useState(true);
  const [mask, setMask] = useState("");
  const [plan, setPlan] = useState<ClonePlan>();
  const [planErr, setPlanErr] = useState("");
  const job = useJob(api);
  useEffect(() => { if (conn && !conn.schemas.includes(owner)) setOwner(conn.schemas[0] ?? ""); }, [conn, owner]);
  useEffect(() => { setName((n) => n || `${owner.toLowerCase()}_${today()}`); }, [owner]);
  const tables = useAsync<TableBrief[]>(() => (connId && owner ? api.sandboxTables(connId, owner) : Promise.resolve([])), [api, connId, owner]);
  useEffect(() => { setPicked(new Set()); setPlan(undefined); }, [connId, owner]);

  const spec = useMemo((): CloneSpec => ({ tables: [...picked], all, rows_per_table: limitOn ? Math.max(1, limit) : null, include_fk_parents: parents, mask: mask.split(/[\n,]/).map((x) => x.trim()).filter(Boolean), with_indexes: indexes }), [picked, all, limitOn, limit, parents, mask, indexes]);
  const ready = !!connId && !!owner && (all || picked.size > 0);
  const seq = useRef(0);
  useEffect(() => {
    if (!ready) { setPlan(undefined); setPlanErr(""); return; }
    const id = ++seq.current;
    const t = setTimeout(() => api.sandboxPlan(connId, owner, spec).then((p) => { if (id === seq.current) { setPlan(p); setPlanErr(""); } }, (e) => { if (id === seq.current) { setPlan(undefined); setPlanErr(errText(e)); } }), 250);
    return () => clearTimeout(t);
  }, [api, connId, owner, spec, ready]);

  useEffect(() => { if (job.done?.ok && job.done.report) onDone((job.done.report as SandboxMeta).name); }, [job.done, onDone]);
  const shown = (tables.data ?? []).filter((t) => t.name.toLowerCase().includes(filter.toLowerCase()));
  const toggle = (n: string) => setPicked((s) => { const x = new Set(s); if (x.has(n)) x.delete(n); else x.add(n); return x; });
  const env = conn?.profile.environment;
  const start = () => void job.start(() => api.sandboxCloneStart(connId, owner, name.trim(), spec));

  if (conns.length === 0) return <div className="muted pad">Connect to a database first: a sandbox is a copy of tables from one.</div>;
  return (
    <div className="sb-new">
      <h3>New sandbox</h3>
      <div className="sb-row">
        <label>From <select aria-label="Connection" value={connId} onChange={(e) => setConnId(e.target.value)}>{conns.map((c) => <option key={c.id} value={c.id}>{c.profile.name}</option>)}</select></label>
        <label>Schema <select aria-label="Schema" value={owner} onChange={(e) => setOwner(e.target.value)}>{(conn?.schemas ?? []).map((s) => <option key={s}>{s}</option>)}</select></label>
        <label>Name <input aria-label="Sandbox name" value={name} onChange={(e) => setName(e.target.value)} /></label>
      </div>
      <div className="sb-scope">
        <label><input type="radio" name="scope" checked={!all} onChange={() => setAll(false)} /> Specific tables</label>
        <label><input type="radio" name="scope" checked={all} onChange={() => setAll(true)} /> Entire schema ({tables.data?.length ?? "…"} tables)</label>
      </div>
      {!all && (
        <div className="sb-tables">
          <div className="sb-row"><input aria-label="Filter tables" placeholder="Filter tables…" value={filter} onChange={(e) => setFilter(e.target.value)} />
            <button className="ghost" onClick={() => setPicked(new Set([...picked, ...shown.map((t) => t.name)]))}>Select shown</button>
            <button className="ghost" onClick={() => setPicked(new Set())}>None</button><span className="muted small">{picked.size} selected</span></div>
          <div className="sb-list">
            {tables.loading && <div className="muted pad">Loading tables…</div>}
            {tables.error && <div className="error pad">{tables.error}</div>}
            {shown.map((t) => <label key={t.name} className="sb-tbl"><input type="checkbox" checked={picked.has(t.name)} onChange={() => toggle(t.name)} /> {t.name}<span className="muted small">{t.rows === null ? "no statistics" : `${t.rows.toLocaleString()} rows`}</span></label>)}
          </div>
        </div>
      )}
      <fieldset className="sb-opts">
        <legend>What to copy</legend>
        <label><input type="checkbox" checked={limitOn} onChange={(e) => setLimitOn(e.target.checked)} /> Only the first <input type="number" aria-label="Rows per table" min={1} value={limit} disabled={!limitOn} onChange={(e) => setLimit(Number(e.target.value))} /> rows of each table</label>
        {!all && <label><input type="checkbox" checked={parents} onChange={(e) => setParents(e.target.checked)} /> Also copy the tables they point to (foreign keys), so joins work</label>}
        <label><input type="checkbox" checked={indexes} onChange={(e) => setIndexes(e.target.checked)} /> Copy indexes</label>
        <label className="col">Leave these columns empty (one per line: COLUMN, TABLE.COLUMN, PAY*)
          <textarea aria-label="Mask columns" rows={2} value={mask} onChange={(e) => setMask(e.target.value)} placeholder="SALARY&#10;EMPLOYEES.SSN" /></label>
      </fieldset>
      {planErr && <div className="error">{planErr}</div>}
      {plan && (
        <div className="sb-plan" data-testid="sb-plan">
          <b>{plan.tables.length} table{plan.tables.length === 1 ? "" : "s"}, about {plan.rows_to_copy.toLocaleString()} rows</b> <span className="muted small">(row counts come from optimizer statistics)</span>
          <div className="small">{plan.tables.map((t) => <span key={t.name} className={`sb-chip ${t.added_as_parent ? "parent" : ""}`} title={t.added_as_parent ? "added because a chosen table points to it" : ""}>{t.name} · {t.rows_to_copy?.toLocaleString() ?? "?"}</span>)}</div>
        </div>
      )}
      <div className={`sb-notice ${env === "prod" ? "prod" : ""}`}>
        This reads rows from <b>{conn?.profile.name}</b>{env === "prod" ? " (PRODUCTION)" : ""} and stores them in a file on this computer. Oracle is only read, never changed. Columns you list above are copied empty; LOB and binary columns are always empty. Delete the sandbox when you are done.
      </div>
      {job.startError && <div className="error">{job.startError}</div>}
      {job.running ? <JobBar progress={job.progress} label="Copying" onCancel={() => job.id && void api.jobCancel(job.id).catch(onError)} />
        : <div className="sb-row"><button className="primary" disabled={!ready || !name.trim()} onClick={start}>Create sandbox</button>
            {job.done && !job.done.ok && <span className={job.done.cancelled ? "muted" : "error"}>{job.done.cancelled ? "Cancelled: nothing was kept." : job.done.error}</span>}</div>}
    </div>
  );
}

function SandboxView({ api, info, preset, onChanged, onDeleted, onError }: { api: Api; info: SandboxInfo; preset?: { sql: string; nonce: number }; onChanged(): void; onDeleted(): void; onError(e: unknown): void }) {
  const { meta } = info;
  const [sql, setSql] = useState(preset?.sql ?? `SELECT * FROM ${meta.tables[0]?.name ?? "dual"}`);
  const [res, setRes] = useState<SandboxRun>();
  const [error, setError] = useState("");
  const [status, setStatus] = useState("Ready");
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<"reset" | "delete">();
  const editor = useRef<EditorHandle>();
  const lastPreset = useRef(preset?.nonce);
  useEffect(() => { if (preset && preset.nonce !== lastPreset.current) { lastPreset.current = preset.nonce; setSql(preset.sql); } }, [preset]);

  const runOne = useCallback(async (text: string): Promise<boolean> => {
    setBusy(true); setError("");
    try {
      const r = await api.sandboxRun(meta.name, text);
      setRes(r);
      setStatus(r.affected !== null ? `${r.affected.toLocaleString()} row(s) affected in ${r.elapsed_ms} ms — in the sandbox only` : `${r.rows.length.toLocaleString()} rows in ${r.elapsed_ms} ms${r.truncated ? " (capped)" : ""}`);
      if (r.wrote) onChanged();
      return true;
    } catch (e) { setError(errText(e)); setStatus("Failed"); return false; }
    finally { setBusy(false); }
  }, [api, meta.name, onChanged]);

  const run = useCallback(async (all: boolean) => {
    const h = editor.current; if (!h) return;
    const r = h.request();
    if (r.selection?.trim() && !all) { await runOne(r.selection); return; }
    let ranges: StatementRange[] = [];
    try { ranges = await api.statements(r.doc); } catch { /* run the whole text */ }
    if (ranges.length === 0) { await runOne(r.doc); return; }
    if (all) { let n = 0; for (const s of ranges) { if (!(await runOne(s.text))) break; n++; } setStatus(`Script: ran ${n} of ${ranges.length} statements`); return; }
    await runOne((ranges.find((x) => r.cursor >= x.start && r.cursor <= x.end) ?? [...ranges].reverse().find((x) => x.end <= r.cursor) ?? ranges[0]).text);
  }, [api, runOne]);

  const act = async (what: "reset" | "delete") => {
    setConfirm(undefined);
    try { if (what === "reset") { await api.sandboxReset(meta.name); setRes(undefined); setStatus("Restored to the clone"); onChanged(); } else { await api.sandboxDelete(meta.name); onDeleted(); } } catch (e) { onError(e); }
  };
  const rows = res?.rows ?? [];
  return (
    <div className="sb-view">
      <div className="sb-head">
        <div><b>{meta.name}</b> <span className="muted small">cloned from {meta.source} · {meta.owner} · {timeAgo(meta.taken_at)} · {fmtBytes(info.bytes)}</span></div>
        <span className="grow" />
        <button disabled={!info.modified} onClick={() => setConfirm("reset")} title="Throw away your changes and go back to the clone">Reset to clone</button>
        <button onClick={() => setConfirm("delete")}>Delete</button>
      </div>
      <div className="sb-tabs">{meta.tables.map((t) => (
        <button key={t.name} className="sb-chip btn" title={`${t.rows.toLocaleString()} rows copied${t.truncated ? ` of ${t.source_rows?.toLocaleString()}` : ""}${t.columns.some((c) => c.masked) ? "; masked: " + t.columns.filter((c) => c.masked).map((c) => c.name).join(", ") : ""}`} onClick={() => setSql(`SELECT * FROM ${t.name}`)}>
          {t.name} · {t.rows.toLocaleString()}{t.truncated ? "*" : ""}{t.added_as_parent ? " ↑" : ""}
        </button>))}</div>
      <div className="sb-toolbar">
        <button className="primary" disabled={busy} onClick={() => void run(false)}>▶ Run</button>
        <button disabled={busy} onClick={() => void run(true)}>Run script</button>
        <span className="muted small">SQLite runs this, with common Oracle functions (NVL, DECODE, TO_CHAR, TO_DATE, TRUNC…) and rewrites such as FROM DUAL and FETCH FIRST. Changes stay here.</span>
      </div>
      <div className="sb-editor"><SqlEditor value={sql} onChange={setSql} onRun={(r) => { void run(r.all); }} onReady={(h) => { editor.current = h; }} complete={noComplete} /></div>
      {error && <div className="error" role="alert">{error}</div>}
      {res && res.changes.length > 0 && <div className="sb-trans small muted" data-testid="sb-trans">Rewritten for SQLite: {res.changes.join("; ")}{res.translated ? ` → ${res.translated}` : ""}</div>}
      <div className="sb-results">
        {res && res.columns.length > 0 ? <ResultGrid columns={asColumns(res.columns)} rows={rows} onNearEnd={() => {}} badges={{}} selectedColumn={undefined} onSelectColumn={() => {}} /> : <div className="empty-note">Run a query. Anything you change here is undone with Reset to clone.</div>}
      </div>
      <footer className="ws-status"><span>{status}</span>{info.modified && <span className="warn">changed since the clone</span>}</footer>
      {confirm && (
        <div className="modal-back"><div className="modal" role="dialog" aria-label={confirm === "reset" ? "Reset sandbox" : "Delete sandbox"}>
          <h3>{confirm === "reset" ? "Reset to the clone?" : `Delete ${meta.name}?`}</h3>
          <p>{confirm === "reset" ? "Your changes in this sandbox are discarded." : "The copy on this computer is removed. Oracle is not affected."}</p>
          <div className="modal-actions"><button onClick={() => setConfirm(undefined)}>Cancel</button><button className="primary" onClick={() => void act(confirm)}>{confirm === "reset" ? "Reset" : "Delete"}</button></div>
        </div></div>
      )}
    </div>
  );
}
