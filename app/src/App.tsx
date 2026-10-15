import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { apiPromise, isDemo, type Api } from "./api";
import type { Analysis, Column, Hit, Profile, Row } from "./types";
import { SqlEditor } from "./components/SqlEditor";
import { ResultGrid } from "./components/ResultGrid";
import { SchemaTree } from "./components/SchemaTree";
import { ConfirmDialog, ConnectDialog } from "./components/Dialogs";
import { InsightPanel, type Target } from "./components/InsightPanel";
import { SearchPalette } from "./components/SearchPalette";
import { InsightCache, badgesForQuery, type Badge, type QueryBadges } from "./badges";

const PAGE = 500;
const msg = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));

interface Result { columns: Column[]; rows: Row[]; handle: number | null; done: boolean; truncated: boolean }

export default function App() {
  const [api, setApi] = useState<Api>();
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [profile, setProfile] = useState<Profile>();
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useState("");

  const [sql, setSql] = useState("SELECT * FROM hr.employees");
  const [result, setResult] = useState<Result>();
  const [status, setStatus] = useState("Ready");
  const [error, setError] = useState("");
  const [running, setRunning] = useState(false);
  const [pending, setPending] = useState<{ sql: string; analysis: Analysis }>();
  const [owner, setOwner] = useState("");
  const [target, setTarget] = useState<Target>();
  const [searchOpen, setSearchOpen] = useState(false);
  const [qb, setQb] = useState<QueryBadges>({ badges: {}, colTable: {} });
  const cache = useMemo(() => (api ? new InsightCache(api) : undefined), [api]);
  const loadingMore = useRef(false);
  const handleRef = useRef<number | null>(null);

  useEffect(() => {
    apiPromise.then(async (a) => { setApi(a); setProfiles(await a.listProfiles()); });
  }, []);

  const onError = useCallback((e: unknown) => setError(msg(e)), []);

  // Load the catalog (autocomplete + search index) whenever the schema changes.
  const refresh = useCallback(async (includeSource: boolean) => {
    if (!api || !owner) return;
    setStatus(includeSource ? "Indexing source…" : "Loading catalog…");
    try {
      const st = await api.refreshMetadata(owner, includeSource);
      setStatus(`Indexed ${st.tables} tables, ${st.columns} columns${includeSource ? " + source" : ""} for ${owner}`);
    } catch (e) { setError(msg(e)); }
  }, [api, owner]);
  useEffect(() => { if (profile) { cache?.clear(); refresh(false); } }, [profile, owner, cache, refresh]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setSearchOpen(true); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  const pick = (h: Hit) => {
    setSearchOpen(false);
    if (h.kind === "table" || h.kind === "view") { setTarget({ owner: h.owner, table: h.name }); setSql(`SELECT * FROM ${h.owner}.${h.name}`); }
    else if (h.kind === "column") setTarget({ owner: h.owner, table: h.parent, column: h.name });
    else setStatus(`${h.parent.toLowerCase()} ${h.owner}.${h.name}, line ${h.line}`);
  };

  const releaseCursor = useCallback(async (a: Api) => {
    const h = handleRef.current;
    handleRef.current = null;
    if (h !== null) await a.closeQuery(h).catch(() => {});
  }, []);

  const run = useCallback(async (text: string, confirmed = false) => {
    if (!api || !text.trim()) return;
    setRunning(true); setError(""); setPending(undefined);
    const t0 = performance.now();
    try {
      await releaseCursor(api);
      const r = await api.runQuery(text, confirmed, PAGE);
      const secs = ((performance.now() - t0) / 1000).toFixed(2);
      if (r.kind === "needs_confirmation") { setPending({ sql: text, analysis: r.analysis }); setStatus("Waiting for confirmation"); }
      else if (r.kind === "rows") {
        handleRef.current = r.done ? null : r.handle;
        setResult({ columns: r.columns, rows: r.rows, handle: r.done ? null : r.handle, done: r.done, truncated: false });
        setQb({ badges: {}, colTable: {} });
        if (r.analysis.kind === "select" && cache) {
          // Badges are decoration: never let a failure here affect the query result.
          api.references(text).then((refs) => badgesForQuery(cache, refs, owner)).then(setQb).catch(() => {});
        }
        setStatus(`${r.rows.length.toLocaleString()} rows${r.done ? "" : " (more available)"} in ${secs}s`);
      } else if (r.kind === "affected") {
        setResult(undefined);
        setStatus(`${r.count.toLocaleString()} row(s) affected in ${secs}s — not committed`);
      } else { setResult(undefined); setStatus(`Statement completed in ${secs}s`); }
    } catch (e) { setError(msg(e)); setStatus("Failed"); }
    finally { setRunning(false); }
  }, [api, releaseCursor, cache, owner]);

  const loadMore = useCallback(async () => {
    const h = handleRef.current;
    if (!api || h === null || loadingMore.current) return;
    loadingMore.current = true;
    try {
      const p = await api.fetchMore(h, PAGE);
      if (handleRef.current !== h) return; // a newer query replaced this one
      if (p.done) handleRef.current = null;
      setResult((r) => {
        if (!r) return r;
        const rows = r.rows.concat(p.rows);
        setStatus(`${rows.length.toLocaleString()} rows${p.truncated ? " — row limit reached" : p.done ? "" : " (more available)"}`);
        return { ...r, rows, done: p.done, truncated: p.truncated, handle: p.done ? null : h };
      });
    } catch (e) { setError(msg(e)); }
    finally { loadingMore.current = false; }
  }, [api]);

  const tx = async (kind: "commit" | "rollback") => {
    try { await api![kind](); setStatus(kind === "commit" ? "Committed" : "Rolled back"); } catch (e) { setError(msg(e)); }
  };

  if (!api) return null;

  if (!profile) {
    return (
      <ConnectDialog
        profiles={profiles} error={connectError} busy={connecting}
        onSave={async (p) => { await api.saveProfile(p); setProfiles(await api.listProfiles()); }}
        onConnect={async (p, pw) => {
          setConnecting(true); setConnectError("");
          try { await api.connect(p, pw); setProfile(p); } catch (e) { setConnectError(msg(e)); }
          finally { setConnecting(false); }
        }}
      />
    );
  }

  return (
    <div className={`app env-${profile.environment}`}>
      <header>
        <strong>{profile.name}</strong>
        <span className="env-badge">{profile.environment.toUpperCase()}</span>
        <span className="muted">{profile.user}@{profile.host}:{profile.port}/{profile.service}</span>
        {isDemo && <span className="demo">DEMO — no database</span>}
        <span className="spacer" />
        <button className="primary" disabled={running} onClick={() => run(sql)} title="Ctrl/Cmd+Enter or F5">▶ Run</button>
        <button disabled={!running} onClick={() => api.cancelQuery().catch(onError)}>■ Stop</button>
        <button onClick={() => setSearchOpen(true)} title="Ctrl/Cmd+K">🔍 Search</button>
        <button onClick={() => refresh(true)} title="Re-read the catalog and index PL/SQL source">Reindex</button>
        <button onClick={() => tx("commit")}>Commit</button>
        <button onClick={() => tx("rollback")}>Rollback</button>
        <button onClick={async () => { await releaseCursor(api); await api.disconnect(); setProfile(undefined); setResult(undefined); }}>Disconnect</button>
      </header>
      <div className="body">
        <SchemaTree api={api} owner={owner} onOwnerChange={setOwner} onError={onError} onSelectTable={(o, t, c) => setTarget({ owner: o, table: t, column: c })} onInsert={(t) => setSql((s) => (t.startsWith("SELECT") ? t : s + t))} />
        <main>
          <SqlEditor value={sql} onChange={setSql} onRun={(t) => run(t)} complete={(text, pos) => api.complete(text, pos, owner)} />
          {error && <div className="error" role="alert">{error}</div>}
          <div className="results">
            {result ? <ResultGrid columns={result.columns} rows={result.rows} onNearEnd={loadMore} badges={qb.badges as Record<string, Badge[]>} selectedColumn={target?.column}
                onSelectColumn={(name) => { const t = qb.colTable[name]; if (t) setTarget({ ...t, column: name }); }} /> : <div className="empty muted">Run a query with Ctrl/Cmd+Enter</div>}
          </div>
          <footer>
            <span>{status}</span>
            {result && !result.done && <button onClick={loadMore}>Load more</button>}
            {result?.truncated && <span className="warn">Result capped at the row limit</span>}
          </footer>
        </main>
        {target && cache && (
          <InsightPanel api={api} cache={cache} env={profile.environment} target={target} onSelect={setTarget} onError={onError} onClose={() => setTarget(undefined)}
            onSaved={() => { refresh(false); }} />
      )}
      </div>
      {searchOpen && <SearchPalette api={api} owner={owner} onPick={pick} onClose={() => setSearchOpen(false)} />}
      {pending && <ConfirmDialog analysis={pending.analysis} onCancel={() => { setPending(undefined); setStatus("Cancelled"); }} onRun={() => run(pending.sql, true)} />}
    </div>
  );
}
