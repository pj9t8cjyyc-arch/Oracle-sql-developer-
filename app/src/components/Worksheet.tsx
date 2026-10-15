import { useCallback, useEffect, useRef, useState } from "react";
import type { Api } from "../api";
import { badgesForQuery, type InsightCache, type QueryBadges } from "../badges";
import { errText, useAsync } from "../hooks";
import { Icon } from "../ui-icons";
import type { Analysis, Column, ObjectKind, Row, StatementRange } from "../types";
import { ColumnFocus } from "./ColumnFocus";
import { ConfirmDialog } from "./Dialogs";
import type { ConnState } from "./Explorer";
import { ResultGrid } from "./ResultGrid";
import { SqlEditor, type EditorHandle, type RunRequest } from "./SqlEditor";

/** `id` is also the worksheet's stable uuid in the local store. `connId` is empty until a connection with `connKey` is open. */
export interface SheetTab { id: string; kind: "sheet"; connId: string; connKey: string; connName: string; owner: string; sql: string; title: string; touched: boolean }

const PAGE = 500;

interface Result { columns: Column[]; rows: Row[]; handle: number | null; done: boolean; truncated: boolean }

interface Props {
  api: Api;
  conns: ConnState[];
  tab: SheetTab;
  getCache(connId: string): InsightCache;
  onPatch(p: Partial<SheetTab>): void;
  onOpen(connId: string, kind: ObjectKind, owner: string, name: string, parent?: string): void;
  onSchemaOpened(connId: string, owner: string): void;
  onError(e: unknown): void;
}

function FocusDrawer({ api, conn, cache, target, onOpen, onClose, onError }: {
  api: Api; conn: ConnState; cache: InsightCache; target: { owner: string; table: string; column: string };
  onOpen: Props["onOpen"]; onClose(): void; onError(e: unknown): void;
}) {
  const res = useAsync(() => cache.get(target.owner, target.table), [cache, target.owner, target.table]);
  const [rev, setRev] = useState(0);
  void rev;
  if (res.error) return <aside className="drawer"><div className="error pad">{res.error}</div></aside>;
  return (
    <aside className="drawer">
      {res.data ? (
        <ColumnFocus api={api} conn={conn.id} env={conn.profile.environment} t={res.data} column={target.column} onClose={onClose}
          onOpen={(k, o, n, p) => onOpen(conn.id, k, o, n, p)} onError={onError}
          onSaved={() => { cache.invalidate(target.owner, target.table); res.reload(); setRev((r) => r + 1); }} />
      ) : <div className="muted pad">Loading…</div>}
    </aside>
  );
}

export function Worksheet({ api, conns, tab, getCache, onPatch, onOpen, onSchemaOpened, onError }: Props) {
  const conn = conns.find((c) => c.id === tab.connId);
  const [result, setResult] = useState<Result>();
  const [status, setStatus] = useState("Ready");
  const [error, setError] = useState("");
  const [running, setRunning] = useState(false);
  const [pending, setPending] = useState<{ sql: string; analysis: Analysis }>();
  const [qb, setQb] = useState<QueryBadges>({ badges: {}, colTable: {} });
  const [focus, setFocus] = useState<{ owner: string; table: string; column: string }>();
  const editor = useRef<EditorHandle>();
  const loadingMore = useRef(false);
  const handleRef = useRef<number | null>(null);
  const connId = tab.connId;

  // A different connection/schema means stale results and badges.
  useEffect(() => { setQb({ badges: {}, colTable: {} }); setFocus(undefined); }, [connId, tab.owner]);

  const release = useCallback(async () => {
    const h = handleRef.current;
    handleRef.current = null;
    if (h !== null) await api.closeQuery(connId, h).catch(() => {});
  }, [api, connId]);

  /** Resolves true when the statement ran (rows, affected or done). */
  const run = useCallback(async (text: string, confirmed = false): Promise<boolean> => {
    if (!conn || !text.trim()) return false;
    let okRun = false;
    setRunning(true); setError(""); setPending(undefined);
    const t0 = performance.now();
    try {
      await release();
      const r = await api.runQuery(connId, text, confirmed, PAGE, tab.owner, tab.id);
      const secs = ((performance.now() - t0) / 1000).toFixed(2);
      if (r.kind === "needs_confirmation") { setPending({ sql: text, analysis: r.analysis }); setStatus("Waiting for confirmation"); }
      else if (r.kind === "rows") {
        handleRef.current = r.done ? null : r.handle;
        setResult({ columns: r.columns, rows: r.rows, handle: r.done ? null : r.handle, done: r.done, truncated: false });
        setQb({ badges: {}, colTable: {} });
        okRun = true;
        setStatus(`${r.rows.length.toLocaleString()} rows${r.done ? "" : " (more available)"} in ${secs}s`);
        if (r.analysis.kind === "select") {
          // Badges are decoration: never let a failure here affect the query result.
          api.references(text).then((refs) => badgesForQuery(getCache(connId), refs, tab.owner)).then(setQb).catch(() => {});
        }
      } else if (r.kind === "affected") { okRun = true; setResult(undefined); setStatus(`${r.count.toLocaleString()} row(s) affected in ${secs}s — not committed`); }
      else { okRun = true; setResult(undefined); setStatus(`Statement completed in ${secs}s`); }
    } catch (e) { setError(errText(e)); setStatus("Failed"); }
    finally { setRunning(false); }
    return okRun;
  }, [api, conn, connId, release, getCache, tab.owner, tab.id]);

  /** Selection wins; otherwise the statement under the cursor; F5 runs every statement in order and stops at the first one that fails. */
  const runRequest = useCallback(async (r: RunRequest) => {
    if (r.selection?.trim()) { await run(r.selection); return; }
    let ranges: StatementRange[] = [];
    try { ranges = await api.statements(r.doc); } catch { /* fall back to the whole text */ }
    if (ranges.length === 0) { await run(r.doc); return; }
    if (r.all && ranges.length > 1) {
      let done = 0;
      for (const st of ranges) { if (!(await run(st.text))) break; done++; }
      setStatus(`Script: ran ${done} of ${ranges.length} statements${done < ranges.length ? ` — stopped at statement ${done + 1}` : ""}`);
      return;
    }
    const at = ranges.find((x) => r.cursor >= x.start && r.cursor <= x.end) ?? [...ranges].reverse().find((x) => x.end <= r.cursor) ?? ranges[0];
    await run(at.text);
  }, [api, run]);

  const loadMore = useCallback(async () => {
    const h = handleRef.current;
    if (h === null || loadingMore.current) return;
    loadingMore.current = true;
    try {
      const p = await api.fetchMore(connId, h, PAGE);
      if (handleRef.current !== h) return;
      if (p.done) handleRef.current = null;
      setResult((r) => {
        if (!r) return r;
        const rows = r.rows.concat(p.rows);
        setStatus(`${rows.length.toLocaleString()} rows${p.truncated ? " — row limit reached" : p.done ? "" : " (more available)"}`);
        return { ...r, rows, done: p.done, truncated: p.truncated, handle: p.done ? null : h };
      });
    } catch (e) { setError(errText(e)); }
    finally { loadingMore.current = false; }
  }, [api, connId]);

  const tx = async (kind: "commit" | "rollback") => {
    try { await api[kind](connId); setStatus(kind === "commit" ? "Committed" : "Rolled back"); } catch (e) { setError(errText(e)); }
  };

  if (!conn) {
    return (
      <div className="worksheet">
        <div className="ws-banner">
          <Icon name="plug" size={16} />
          <span>
            {tab.connName ? <>This worksheet was saved for <b>{tab.connName}</b>, which is not connected.</> : "This worksheet's connection was closed."}{" "}
            Your SQL is kept. Connect to run it{conns.length > 0 ? ", or run it on:" : "."}
          </span>
          {conns.length > 0 && (
            <select value="" aria-label="Use connection" onChange={(e) => { const c = conns.find((x) => x.id === e.target.value); if (c) onPatch({ connId: c.id, connKey: c.key, connName: c.profile.name, owner: c.schemas[0] ?? c.user }); }}>
              <option value="" disabled>choose connection…</option>
              {conns.map((c) => <option key={c.id} value={c.id}>{c.profile.name}</option>)}
            </select>
          )}
        </div>
        <div className="ws-body"><div className="ws-main">
          <SqlEditor value={tab.sql} onChange={(sql) => onPatch({ sql })} onRun={() => {}} complete={async () => ({ from: 0, items: [] })} />
        </div></div>
      </div>
    );
  }
  const env = conn.profile.environment;

  return (
    <div className="worksheet">
      <div className="ws-toolbar">
        <select value={connId} aria-label="Connection" className={`conn-select env-${env}`} onChange={(e) => { const c = conns.find((x) => x.id === e.target.value)!; setResult(undefined); onPatch({ connId: c.id, connKey: c.key, connName: c.profile.name, owner: c.schemas[0] ?? c.user }); }}>
          {conns.map((c) => <option key={c.id} value={c.id}>{c.profile.name}</option>)}
        </select>
        <select value={tab.owner} aria-label="Schema" onChange={(e) => { onPatch({ owner: e.target.value }); onSchemaOpened(connId, e.target.value); }}>
          {conn.schemas.map((s) => <option key={s}>{s}</option>)}
        </select>
        <span className="sep" />
        <button className="primary" disabled={running} onClick={() => editor.current?.run(false)} title="Runs the selection, or the statement at the cursor (Ctrl/Cmd+Enter)">▶ Run</button>
        <button disabled={running} onClick={() => editor.current?.run(true)} title="Runs every statement in order and stops at the first failure (F5)">Run script</button>
        <button disabled={!running} onClick={() => api.cancelQuery(connId).catch(onError)}><Icon name="stop" size={12} /> Stop</button>
        <span className="sep" />
        <button onClick={() => tx("commit")} title="Shared by every worksheet on this connection">Commit</button>
        <button onClick={() => tx("rollback")}>Rollback</button>
      </div>
      <div className="ws-body">
        <div className="ws-main">
          <SqlEditor value={tab.sql} onChange={(sql) => onPatch({ sql })} onRun={runRequest} onReady={(h) => { editor.current = h; }} complete={(text, pos) => api.complete(connId, text, pos, tab.owner)} suggestHistory={(typed) => api.historySuggest(typed, conn.key, 5)} />
          {error && <div className="error" role="alert">{error}</div>}
          <div className="results">
            {result
              ? <ResultGrid columns={result.columns} rows={result.rows} onNearEnd={loadMore} badges={qb.badges} selectedColumn={focus?.column}
                  onSelectColumn={(name) => { const t = qb.colTable[name]; if (t) setFocus({ ...t, column: name }); }} />
              : <div className="empty-note">Run a query with Ctrl/Cmd+Enter. Click a result column header to see its indexes, triggers and sequences.</div>}
          </div>
          <footer className="ws-status">
            <span>{status}</span>
            {result && !result.done && <button className="ghost" onClick={loadMore}>Load more</button>}
            {result?.truncated && <span className="warn">Result capped at the row limit</span>}
          </footer>
        </div>
        {focus && <FocusDrawer api={api} conn={conn} cache={getCache(connId)} target={focus} onOpen={onOpen} onClose={() => setFocus(undefined)} onError={onError} />}
      </div>
      {pending && <ConfirmDialog analysis={pending.analysis} onCancel={() => { setPending(undefined); setStatus("Cancelled"); }} onRun={() => run(pending.sql, true)} />}
    </div>
  );
}
