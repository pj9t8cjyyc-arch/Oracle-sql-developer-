import { useCallback, useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { errText, useAsync } from "../hooks";
import { fmtBytes, fmtDateTime, fmtMs, timeAgo } from "../format";
import { Icon } from "../ui-icons";
import type { HistoryHit, HistoryQuery, Step, WorksheetRec } from "../types";
import type { ConnState } from "./Explorer";

const DAY = 86_400_000;
const RANGES: [string, number | null][] = [["Any time", null], ["Last 7 days", 7], ["Last 30 days", 30], ["Last 90 days", 90], ["Last year", 365]];
const KINDS = ["select", "insert", "update", "delete", "ddl", "plsql"];

interface Props {
  api: Api;
  conns: ConnState[];
  onOpenSql(sql: string, connKey: string): void;
  onInsertSql(sql: string): void;
  onOpenWorksheet(ws: WorksheetRec): void;
  canInsert: boolean;
  /** The History tab is visible. Panes stay mounted, so it refreshes whenever it comes back. */
  active: boolean;
  onError(e: unknown): void;
}

/** Render FTS snippet markers `[word]` as <mark>. */
function Marked({ text }: { text: string }) {
  return <>{text.split(/(\[[^\]]*\])/).map((p, i) => (p.startsWith("[") && p.endsWith("]") ? <mark key={i}>{p.slice(1, -1)}</mark> : <span key={i}>{p}</span>))}</>;
}

function QueryCard({ h, api, conns, canInsert, onOpenSql, onInsertSql, onChanged, onError }: {
  h: HistoryHit; api: Api; conns: ConnState[]; canInsert: boolean; onOpenSql: Props["onOpenSql"]; onInsertSql: Props["onInsertSql"]; onChanged(): void; onError(e: unknown): void;
}) {
  const [copied, setCopied] = useState(false);
  const [confirmDel, setConfirmDel] = useState(false);
  const lines = h.sql.split("\n").length;
  const live = conns.some((c) => c.key === h.last_conn_key);
  return (
    <article className="qcard" data-hash={h.hash}>
      <div className="qcard-head">
        <span className="chip">{h.kind}</span>
        <span className="chip">{h.last_conn_name}</span>
        <span className="meta">
          ran <b>{h.runs}×</b> · last {timeAgo(h.last_run)} <span title={fmtDateTime(h.last_run)}>({new Date(h.last_run).toLocaleDateString()})</span>
          {h.avg_ms !== null && <> · avg {fmtMs(h.avg_ms)}</>}
          {h.last_status !== "ok" && <> · <span className={`status-${h.last_status}`}>last run {h.last_status}</span></>}
        </span>
        <span className="grow" />
        <button className="ghost icon" title={h.pinned ? "Unstar" : "Star (keeps it on top)"} aria-label="Star" onClick={() => api.historyPin(h.hash, !h.pinned).then(onChanged).catch(onError)}>
          <Icon name="star" size={15} className={h.pinned ? "pinned-star" : ""} />
        </button>
      </div>
      <pre className={`qsql ${lines > 7 ? "clip" : ""}`}>{h.snippet ? <Marked text={h.sql.length === h.snippet.replace(/[[\]]/g, "").length ? h.snippet : h.sql} /> : h.sql}</pre>
      <div className="qfoot">
        <span className="tables">{h.tables.map((t) => <span key={t} className="chip">{t}</span>)}</span>
        <button className="primary" disabled={!live} title={live ? "" : `Connect to ${h.last_conn_name} to run this`} onClick={() => onOpenSql(h.sql, h.last_conn_key)}><Icon name="sql" size={14} /> Open in worksheet</button>
        <button disabled={!canInsert} title={canInsert ? "" : "Open a worksheet first"} onClick={() => onInsertSql(h.sql)}>Insert</button>
        <button onClick={() => { void navigator.clipboard?.writeText(h.sql).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1200); }).catch(() => {}); }}><Icon name={copied ? "check" : "copy"} size={14} /> {copied ? "Copied" : "Copy"}</button>
        {confirmDel ? (
          <>
            <span className="muted small">Forget all {h.runs} runs?</span>
            <button className="danger" onClick={() => api.historyForget(h.hash).then(onChanged).catch(onError)}>Forget</button>
            <button className="ghost" onClick={() => setConfirmDel(false)}>Keep</button>
          </>
        ) : <button className="ghost icon" title="Forget this query" aria-label="Forget" onClick={() => setConfirmDel(true)}><Icon name="trash" size={14} /></button>}
      </div>
    </article>
  );
}

function Steps({ api, uuid }: { api: Api; uuid: string }) {
  const res = useAsync(() => api.wsSteps(uuid), [api, uuid]);
  if (res.loading) return <div className="steps muted">Loading…</div>;
  const steps: Step[] = res.data ?? [];
  if (steps.length === 0) return <div className="steps muted">Nothing was run from this worksheet yet.</div>;
  return (
    <div className="steps">
      {steps.map((s, i) => (
        <div key={i} className="step">
          <span className={`status-${s.status}`}>{s.status === "ok" ? "●" : s.status === "error" ? "✕" : "◌"}</span>
          <span className="muted">{fmtDateTime(s.at)}</span>
          <span className="chip">{s.kind}</span>
          <span className="sql" title={s.sql}>{s.sql.replace(/\s+/g, " ")}</span>
          <span className="muted">{fmtMs(s.duration_ms)}{s.rows !== null ? ` · ${s.rows} rows` : ""}</span>
          {s.error && <span className="step-err">{s.error}</span>}
        </div>
      ))}
    </div>
  );
}

function WorksheetCard({ w, api, onOpen, onChanged, onError }: { w: WorksheetRec; api: Api; onOpen(w: WorksheetRec): void; onChanged(): void; onError(e: unknown): void }) {
  const [steps, setSteps] = useState(false);
  const [confirmDel, setConfirmDel] = useState(false);
  return (
    <article className="qcard" data-uuid={w.uuid}>
      <div className="qcard-head">
        <strong>{w.title}</strong>
        <span className="chip">{w.conn_name}</span>
        <span className={`chip ${w.closed_at === null ? "ok" : ""}`}>{w.closed_at === null ? "open" : `closed ${timeAgo(w.closed_at)}`}</span>
        <span className="meta">edited {timeAgo(w.updated_at)} · {w.runs} run{w.runs === 1 ? "" : "s"} · {w.outline.length} statement{w.outline.length === 1 ? "" : "s"}</span>
        <span className="grow" />
        <button className="ghost icon" aria-label="Star" title="Star" onClick={() => api.wsPin(w.uuid, !w.pinned).then(onChanged).catch(onError)}><Icon name="star" size={15} className={w.pinned ? "pinned-star" : ""} /></button>
      </div>
      <div className="outline">
        {w.outline.slice(0, 8).map((o, i) => <span key={i} className="chip" title={o.preview}>{i + 1}. {o.kind}{o.tables ? ` · ${o.tables.split(" ").slice(0, 2).join(", ")}` : ""}</span>)}
        {w.outline.length > 8 && <span className="muted small">+{w.outline.length - 8} more</span>}
      </div>
      <div className="qfoot">
        <span className="tables" />
        <button className="primary" onClick={() => onOpen(w)}>{w.closed_at === null ? "Go to worksheet" : "Reopen"}</button>
        <button onClick={() => setSteps((s) => !s)}>{steps ? "Hide steps" : `Steps (${w.runs})`}</button>
        {confirmDel ? (
          <>
            <span className="muted small">Delete this worksheet? (its run history stays)</span>
            <button className="danger" onClick={() => api.wsDelete(w.uuid).then(onChanged).catch(onError)}>Delete</button>
            <button className="ghost" onClick={() => setConfirmDel(false)}>Keep</button>
          </>
        ) : <button className="ghost icon" aria-label="Delete worksheet" title="Delete worksheet" onClick={() => setConfirmDel(true)}><Icon name="trash" size={14} /></button>}
      </div>
      {steps && <Steps api={api} uuid={w.uuid} />}
    </article>
  );
}

export function HistoryPage({ api, conns, onOpenSql, onInsertSql, onOpenWorksheet, canInsert, active, onError }: Props) {
  const [mode, setMode] = useState<"queries" | "worksheets">("queries");
  const [text, setText] = useState("");
  const [connKey, setConnKey] = useState("");
  const [rangeDays, setRangeDays] = useState<number | null>(null);
  const [onlyOk, setOnlyOk] = useState(true);
  const [kinds, setKinds] = useState<string[]>([]);
  const [starred, setStarred] = useState(false);
  const [rev, setRev] = useState(0);
  const [debounced, setDebounced] = useState("");
  useEffect(() => { const t = setTimeout(() => setDebounced(text), 150); return () => clearTimeout(t); }, [text]);
  const reload = useCallback(() => setRev((r) => r + 1), []);
  useEffect(() => { if (active) reload(); }, [active, reload]);

  const query: HistoryQuery = useMemo(() => ({
    text: debounced, conn_key: connKey || null, only_ok: onlyOk, since: rangeDays ? Date.now() - rangeDays * DAY : null, kinds, pinned_only: starred, limit: 60,
  }), [debounced, connKey, onlyOk, rangeDays, kinds, starred]);

  const hits = useAsync(() => (mode === "queries" ? api.historySearch(query) : Promise.resolve([] as HistoryHit[])), [api, mode, query, rev]);
  const sheets = useAsync(() => (mode === "worksheets" ? api.wsList({ state: "all", text: debounced, conn_key: connKey || null, limit: 100 }) : Promise.resolve([] as WorksheetRec[])), [api, mode, debounced, connKey, rev]);
  const status = useAsync(() => api.historyStatus(), [api, rev]);
  const st = status.data;
  const list = mode === "queries" ? hits : sheets;

  return (
    <div className="history-page">
      <div className="hist-head">
        <Icon name="history" size={22} />
        <div>
          <h2>History</h2>
          {st && <div className="hist-stats">{st.stats.executions.toLocaleString()} executions · {st.stats.distinct_queries.toLocaleString()} distinct queries · {st.stats.worksheets} worksheets{st.stats.first_at ? ` · since ${timeAgo(st.stats.first_at)}` : ""} · {fmtBytes(st.stats.size_bytes)} on disk</div>}
        </div>
        {st && (
          <button className="rec-toggle" onClick={() => api.historySetEnabled(!st.enabled).then(reload).catch(onError)} title="Pause or resume recording statements">
            {st.enabled ? <span className="rec-on">● Recording</span> : <span className="rec-off"><Icon name="pause" size={13} /> Paused</span>}
          </button>
        )}
      </div>
      {st && !st.persistent && <div className="error banner"><span>The history file could not be opened, so history will be lost when the app closes.</span></div>}
      <div className="segmented">
        <button className={mode === "queries" ? "on" : ""} onClick={() => setMode("queries")}>Queries</button>
        <button className={mode === "worksheets" ? "on" : ""} onClick={() => setMode("worksheets")}>Worksheets</button>
      </div>
      <div className="hist-filters">
        <label className="hist-search"><Icon name="search" size={15} /><input autoFocus placeholder={mode === "queries" ? "Find a query you ran: words, table or column names…" : "Find a worksheet by title or SQL…"} value={text} onChange={(e) => setText(e.target.value)} /></label>
        <select value={connKey} onChange={(e) => setConnKey(e.target.value)} aria-label="Connection"><option value="">All connections</option>{conns.map((c) => <option key={c.key} value={c.key}>{c.profile.name}</option>)}</select>
        {mode === "queries" && (
          <>
            <select value={rangeDays ?? ""} onChange={(e) => setRangeDays(e.target.value ? Number(e.target.value) : null)} aria-label="Time range">{RANGES.map(([l, d]) => <option key={l} value={d ?? ""}>{l}</option>)}</select>
            <button className={`chipbtn ${onlyOk ? "on" : ""}`} onClick={() => setOnlyOk(!onlyOk)}>Successful only</button>
            <button className={`chipbtn ${starred ? "on" : ""}`} onClick={() => setStarred(!starred)}><Icon name="star" size={12} /> Starred</button>
            {KINDS.map((k) => <button key={k} className={`chipbtn ${kinds.includes(k) ? "on" : ""}`} onClick={() => setKinds(kinds.includes(k) ? kinds.filter((x) => x !== k) : [...kinds, k])}>{k}</button>)}
          </>
        )}
      </div>
      {list.error && <div className="error">{list.error}</div>}
      {mode === "queries" && (hits.data ?? []).map((h) => (
        <QueryCard key={h.hash} h={h} api={api} conns={conns} canInsert={canInsert} onOpenSql={onOpenSql} onInsertSql={onInsertSql} onChanged={reload} onError={onError} />
      ))}
      {mode === "worksheets" && (sheets.data ?? []).map((w) => <WorksheetCard key={w.uuid} w={w} api={api} onOpen={onOpenWorksheet} onChanged={reload} onError={onError} />)}
      {!list.loading && !list.error && (list.data ?? []).length === 0 && (
        <div className="hist-empty">{debounced ? `Nothing matches “${debounced}”.` : mode === "queries" ? "No statements recorded yet. Run something and it will show up here." : "No saved worksheets yet."}</div>
      )}
    </div>
  );
  void errText;
}
