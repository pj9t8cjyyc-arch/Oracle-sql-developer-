import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { apiPromise, isDemo, type Api } from "./api";
import { InsightCache } from "./badges";
import { ConnectDialog } from "./components/Dialogs";
import { ComparePage } from "./components/ComparePage";
import { SandboxPage } from "./components/SandboxPage";
import { AiSettings } from "./components/AiSettings";
import { GraphPage } from "./components/GraphPage";
import { exposeAiLogin } from "./ai-login";
import { ImportWizard } from "./components/ImportWizard";
import { PackagesPage } from "./components/PackagesPage";
import { Explorer, type ConnState } from "./components/Explorer";
import { ObjectPage, type ObjTarget } from "./components/ObjectPage";
import { SearchPalette, type ConnHit } from "./components/SearchPalette";
import { HistoryPage } from "./components/HistoryPage";
import { Worksheet, type SheetTab } from "./components/Worksheet";
import { errText } from "./hooks";
import { Icon, KindIcon } from "./ui-icons";
import type { ObjectEntry, ObjectKind, Profile, WorksheetRec } from "./types";

interface ObjTab { id: string; kind: "object"; connId: string; target: ObjTarget }
interface HistTab { id: "history"; kind: "history"; connId: "" }
interface PkgTab { id: string; kind: "packages"; connId: string; owner: string; select?: string }
interface GraphTab { id: string; kind: "graph"; connId: string; owner: string; focus?: string }
interface CmpTab { id: "compare"; kind: "compare"; connId: "" }
interface SbTab { id: "sandbox"; kind: "sandbox"; connId: ""; preset?: { sql: string; nonce: number } }
type Tab = SheetTab | ObjTab | HistTab | PkgTab | GraphTab | CmpTab | SbTab;
const uuid = () => (typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `ws-${Date.now()}-${Math.random().toString(16).slice(2)}`);

const sameObj = (a: ObjTarget, b: ObjTarget) => a.owner === b.owner && a.kind === b.kind && a.name === b.name;
const toKind = (t: string): ObjectKind => (t.replace(" BODY", "") as ObjectKind);
const sig = (sql: string, owner: string, key: string) => `${key}|${owner}|${sql}`;

export default function App() {
  const [api, setApi] = useState<Api>();
  const [importing, setImporting] = useState(false);
  const [aiOpen, setAiOpen] = useState(false);
  const [aiStatus, setAiStatus] = useState<import("./types").AiStatus>();
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [conns, setConns] = useState<ConnState[]>([]);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeId, setActiveId] = useState<string>();
  const [connectOpen, setConnectOpen] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const seq = useRef(0);
  const caches = useRef(new Map<string, InsightCache>());
  const indexed = useRef(new Set<string>());
  const saved = useRef(new Map<string, string>());
  const lastSheetId = useRef<string>();

  useEffect(() => {
    apiPromise.then(async (a) => {
      setApi(a);
      exposeAiLogin(a);
      a.aiStatus().then(setAiStatus).catch(() => {});
      setProfiles(await a.listProfiles());
      setConnectOpen(true);
      // Bring back the worksheets that were open when the app was last closed. They keep
      // their SQL and attach to their connection as soon as it is opened again.
      try {
        const open = await a.wsList({ state: "open", limit: 50 });
        if (open.length) {
          setTabs(open.map((w) => ({ id: w.uuid, kind: "sheet" as const, connId: "", connKey: w.conn_key, connName: w.conn_name, owner: w.owner, sql: w.sql, title: w.title, touched: true })));
          setActiveId(open[0].uuid);
          for (const w of open) saved.current.set(w.uuid, sig(w.sql, w.owner, w.conn_key));
        }
      } catch { /* history is optional: the app works without it */ }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onError = useCallback((e: unknown) => setError(errText(e)), []);
  const getCache = useCallback((connId: string) => {
    let c = caches.current.get(connId);
    if (!c && api) { c = new InsightCache(api, connId); caches.current.set(connId, c); }
    return c!;
  }, [api]);

  // Load the catalog (autocomplete + search index) once per connection and schema.
  const onSchemaOpened = useCallback((connId: string, owner: string) => {
    const k = `${connId}/${owner}`;
    if (!api || indexed.current.has(k)) return;
    indexed.current.add(k);
    api.refreshMetadata(connId, owner, false).then(
      (st) => setNotice(`Indexed ${st.tables} tables, ${st.columns} columns for ${owner}`),
      (e) => { indexed.current.delete(k); onError(e); },
    );
  }, [api, onError]);

  const newSheet = useCallback((connId: string, owner: string, sql = "SELECT * FROM ", touched = false) => {
    const c = conns.find((x) => x.id === connId);
    const id = uuid();
    const n = tabs.filter((t) => t.kind === "sheet").length + 1;
    setTabs((t) => [...t, { id, kind: "sheet", connId, connKey: c?.key ?? "", connName: c?.profile.name ?? "", owner, sql, title: `SQL ${n}`, touched }]);
    setActiveId(id);
  }, [tabs, conns]);

  const openHistory = useCallback(() => {
    setTabs((ts) => (ts.some((t) => t.id === "history") ? ts : [...ts, { id: "history", kind: "history", connId: "" }]));
    setActiveId("history");
  }, []);

  /** One Packages tab and one Graph tab per connection and schema; opening again re-targets it. */
  const openPackages = useCallback((connId: string, owner: string, select?: string) => {
    const id = `pk:${connId}:${owner}`;
    setTabs((ts) => (ts.some((t) => t.id === id) ? ts.map((t) => (t.id === id ? { ...t, select } as Tab : t)) : [...ts, { id, kind: "packages", connId, owner, select }]));
    setActiveId(id);
  }, []);
  const openGraph = useCallback((connId: string, owner: string, focus?: string) => {
    const id = `gr:${connId}:${owner}`;
    setTabs((ts) => (ts.some((t) => t.id === id) ? ts.map((t) => (t.id === id ? { ...t, focus } as Tab : t)) : [...ts, { id, kind: "graph", connId, owner, focus }]));
    setActiveId(id);
  }, []);

  const openCompare = useCallback(() => {
    setTabs((ts) => (ts.some((t) => t.id === "compare") ? ts : [...ts, { id: "compare", kind: "compare", connId: "" }]));
    setActiveId("compare");
  }, []);

  const openSandbox = useCallback((sql?: string) => {
    const preset = sql ? { sql, nonce: Date.now() } : undefined;
    setTabs((ts) => (ts.some((t) => t.id === "sandbox") ? ts.map((t) => (t.id === "sandbox" ? { ...t, preset } as Tab : t)) : [...ts, { id: "sandbox", kind: "sandbox", connId: "", preset }]));
    setActiveId("sandbox");
  }, []);

  const openObject = useCallback((connId: string, kind: ObjectKind, owner: string, name: string, parent?: string | null, entry?: ObjectEntry | null, column?: string) => {
    const target: ObjTarget = { owner, kind, name, parent, entry, column };
    const found = tabs.find((t): t is ObjTab => t.kind === "object" && t.connId === connId && sameObj(t.target, target));
    if (found) {
      setTabs((ts) => ts.map((t) => (t.id === found.id ? { ...found, target: { ...found.target, column: column ?? found.target.column, parent: parent ?? found.target.parent, entry: entry ?? found.target.entry } } : t)));
      setActiveId(found.id);
    } else {
      const id = `t${++seq.current}`;
      setTabs((ts) => [...ts, { id, kind: "object", connId, target }]);
      setActiveId(id);
    }
  }, [tabs]);

  const connect = useCallback(async (p: Profile, password: string) => {
    if (!api) return;
    setConnecting(true); setConnectError("");
    try {
      const info = await api.connect(p, password);
      const schemas = await api.listSchemas(info.id);
      const c: ConnState = { id: info.id, key: info.key, profile: p, schemas: schemas.length ? schemas : [p.user.toUpperCase()], user: p.user };
      setConns((cs) => [...cs, c]);
      setConnectOpen(false);
      const own = c.schemas.find((s) => s.toLowerCase() === p.user.toLowerCase()) ?? c.schemas[0];
      // Restored worksheets that belong to this database attach to it now.
      const waiting = tabs.filter((t): t is SheetTab => t.kind === "sheet" && !t.connId && t.connKey === info.key);
      setTabs((ts) => {
        const bound = ts.map((t) => (t.kind === "sheet" && !t.connId && t.connKey === info.key ? { ...t, connId: info.id, connName: p.name } : t));
        if (waiting.length) return bound;
        const id = uuid();
        setActiveId(id);
        return [...bound, { id, kind: "sheet", connId: info.id, connKey: info.key, connName: p.name, owner: own, sql: `SELECT * FROM ${own}.`, title: `SQL ${ts.filter((t) => t.kind === "sheet").length + 1}`, touched: false }];
      });
      if (waiting.length) setActiveId(waiting[0].id);
    } catch (e) { setConnectError(errText(e)); }
    finally { setConnecting(false); }
  }, [api, tabs]);

  const disconnect = useCallback(async (connId: string) => {
    if (!api) return;
    await api.disconnect(connId).catch(onError);
    caches.current.delete(connId);
    setConns((cs) => cs.filter((c) => c.id !== connId));
    setTabs((ts) => ts.filter((t) => t.connId !== connId || t.kind === "sheet").map((t) => (t.kind === "sheet" && t.connId === connId ? { ...t, connId: "" } : t)));
  }, [api, onError]);

  const closeTab = (id: string) => {
    const t = tabs.find((x) => x.id === id);
    // The worksheet is not deleted: it is archived and stays searchable under History.
    if (t?.kind === "sheet" && api) api.wsClose(id).catch(onError);
    setTabs((ts) => {
      const i = ts.findIndex((t) => t.id === id);
      const rest = ts.filter((t) => t.id !== id);
      if (id === activeId) setActiveId(rest[Math.min(i, rest.length - 1)]?.id);
      return rest;
    });
  };

  const pick = (h: ConnHit) => {
    setSearchOpen(false);
    if (h.kind === "table" || h.kind === "view") openObject(h.connId, h.kind === "view" ? "VIEW" : "TABLE", h.owner, h.name);
    else if (h.kind === "column") openObject(h.connId, "TABLE", h.owner, h.parent, null, null, h.name);
    else openObject(h.connId, toKind(h.parent), h.owner, h.name);
  };

  const pickHistory = (h: import("./types").HistoryHit) => {
    setSearchOpen(false);
    const c = conns.find((x) => x.key === h.last_conn_key) ?? conns.find((x) => x.id === activeConnId) ?? conns[0];
    if (c) newSheet(c.id, h.last_owner || c.schemas[0], h.sql, true);
  };

  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setSearchOpen(true); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  // Autosave every worksheet the user has touched, shortly after they stop typing.
  useEffect(() => {
    if (!api) return;
    const timers: ReturnType<typeof setTimeout>[] = [];
    tabs.forEach((t, position) => {
      if (t.kind !== "sheet" || !t.touched) return;
      const s = sig(t.sql, t.owner, t.connKey);
      if (saved.current.get(t.id) === s) return;
      timers.push(setTimeout(() => {
        api.wsSave({ uuid: t.id, conn_key: t.connKey, conn_name: t.connName, owner: t.owner, title: null, sql: t.sql, position })
          .then(() => saved.current.set(t.id, s))
          .catch(onError);
      }, 700));
    });
    return () => timers.forEach(clearTimeout);
  }, [api, tabs, onError]);

  const active = tabs.find((t) => t.id === activeId);
  const activeObj = active?.kind === "object" ? active : undefined;
  const activeSheet = active?.kind === "sheet" ? active : undefined;
  if (activeSheet) lastSheetId.current = activeSheet.id;
  const connOf = (id: string) => conns.find((c) => c.id === id);
  const connView = useMemo(() => Object.fromEntries(conns.map((c) => [c.id, { id: c.id, name: c.profile.name, env: c.profile.environment, user: c.user }])), [conns]);

  if (!api) return null;
  const activeConnId = active?.connId || conns[0]?.id;
  /** The schema of the current tab, else the connection's own schema. */
  const ctxOwner = (c: ConnState) => (active && "owner" in active && active.connId === c.id && active.owner) || (active?.kind === "object" && active.connId === c.id ? active.target.owner : "") || c.schemas.find((x) => x.toLowerCase() === c.user.toLowerCase()) || c.schemas[0];

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand"><span className="logo"><Icon name="database" size={16} /></span> Ora IDE</div>
        <button className="search-pill" onClick={() => setSearchOpen(true)} disabled={conns.length === 0}>
          <Icon name="search" size={14} /> <span>Search tables, columns, source…</span> <kbd>Ctrl K</kbd>
        </button>
        <button disabled={conns.length === 0} onClick={() => setImporting(true)} title="Load a CSV or Excel file into a table"><Icon name="upload" size={14} /> Import</button>
        <button disabled={conns.length === 0} onClick={() => { const c = connOf(activeConnId ?? "") ?? conns[0]; if (c) openPackages(c.id, ctxOwner(c)); }} title="Every package: members, callers, tables used"><Icon name="package" size={14} /> Packages</button>
        <button disabled={conns.length === 0} onClick={() => { const c = connOf(activeConnId ?? "") ?? conns[0]; if (c) openGraph(c.id, ctxOwner(c)); }} title="What uses what"><Icon name="out" size={14} /> Graph</button>
        <button onClick={openCompare} title="Compare text, data or schemas"><Icon name="columns" size={14} /> Compare</button>
        <button onClick={() => openSandbox()} title="A private SQLite copy of some tables: run and change things without touching Oracle"><Icon name="database" size={14} /> Sandbox</button>
        <button onClick={() => setAiOpen(true)} title={aiStatus?.signed_in && aiStatus.configured ? "AI is ready" : "Set up the AI connection"}>
          <span className={`dot ${aiStatus?.signed_in && aiStatus.configured ? "ok" : aiStatus?.configured ? "warn" : "off"}`} /> AI
        </button>
        <button onClick={openHistory} title="Everything you have run, searchable"><Icon name="history" size={14} /> History</button>
        <span className="spacer" />
        {isDemo && <span className="demo">DEMO · no real database</span>}
        <button className="primary" disabled={conns.length === 0} onClick={() => { const c = connOf(activeConnId ?? "") ?? conns[0]; if (c) newSheet(c.id, c.schemas.find((s) => s.toLowerCase() === c.user.toLowerCase()) ?? c.schemas[0]); }}><Icon name="sql" size={14} /> New worksheet</button>
      </header>
      <div className="shell">
        <Explorer
          api={api} conns={conns} onError={onError} onSchemaOpened={onSchemaOpened}
          active={activeObj ? { connId: activeObj.connId, owner: activeObj.target.owner, kind: activeObj.target.kind, name: activeObj.target.name } : undefined}
          onOpen={(c, owner, kind, entry) => openObject(c, kind, owner, entry.name, entry.parent, entry)}
          onNewSheet={(c, owner) => newSheet(c, owner)} onDisconnect={disconnect} onAdd={() => setConnectOpen(true)}
        />
        <section className="workspace">
          <div className="tabbar" role="tablist">
            {tabs.map((t) => {
              const c = connOf(t.connId);
              const env = c?.profile.environment ?? "dev";
              const noConn = t.kind === "history" || t.kind === "compare" || t.kind === "sandbox";
              return (
                <div key={t.id} role="tab" aria-selected={t.id === activeId} className={`tab ${t.id === activeId ? "on" : ""}`} onClick={() => setActiveId(t.id)} title={c?.profile.name}>
                  <span className={`tab-stripe env-${env}`} style={noConn ? { background: "var(--accent)" } : undefined} />
                  {t.kind === "sheet" ? <Icon name="sql" size={14} /> : t.kind === "history" ? <Icon name="history" size={14} /> : t.kind === "compare" ? <Icon name="columns" size={14} /> : t.kind === "sandbox" ? <Icon name="database" size={14} /> : t.kind === "packages" ? <Icon name="package" size={14} /> : t.kind === "graph" ? <Icon name="out" size={14} /> : <KindIcon kind={t.target.kind} size={14} />}
                  <span className="tab-title">{t.kind === "sheet" ? t.title : t.kind === "history" ? "History" : t.kind === "compare" ? "Compare" : t.kind === "sandbox" ? "Sandbox" : t.kind === "packages" ? `Packages · ${t.owner}` : t.kind === "graph" ? `Graph · ${t.owner}` : t.target.name}</span>
                  {c && conns.length > 1 && <span className="tab-conn">{c.profile.name.split(" ")[0]}</span>}
                  <button className="ghost icon close" aria-label="Close tab" onClick={(e) => { e.stopPropagation(); closeTab(t.id); }}><Icon name="x" size={12} /></button>
                </div>
              );
            })}
          </div>
          {error && <div className="error banner" role="alert"><span>{error}</span><button className="ghost icon" onClick={() => setError("")}><Icon name="x" size={12} /></button></div>}
          <div className="panes">
            {tabs.length === 0 && (
              <div className="welcome">
                <Icon name="database" size={36} />
                <h2>{conns.length ? "Open something" : "Connect to a database"}</h2>
                <p>{conns.length ? "Pick an object in the explorer to see its indexes, triggers and sequences, or start a SQL worksheet." : "You can connect to several Oracle databases at the same time."}</p>
              </div>
            )}
            {tabs.map((t) => (
              <div key={t.id} className="pane" hidden={t.id !== activeId}>
                {t.kind === "sandbox" ? (
                  <SandboxPage api={api} conns={conns} preset={t.preset} onError={onError} />
                ) : t.kind === "compare" ? (
                  <ComparePage api={api} conns={conns} onError={onError}
                    sheets={tabs.filter((x): x is SheetTab => x.kind === "sheet").map((x) => ({ id: x.id, title: x.title, sql: x.sql }))}
                    onWorksheet={(c, o, sql) => newSheet(c, o, sql, true)} />
                ) : t.kind === "packages" ? (
                  connView[t.connId] ? <PackagesPage api={api} conn={t.connId} owner={t.owner} select={t.select} onError={onError}
                    onOpenSource={(p) => openObject(t.connId, "PACKAGE", t.owner, p)} onOpenTable={(n) => openObject(t.connId, "TABLE", t.owner, n)}
                    onGraph={(p) => openGraph(t.connId, t.owner, `PACKAGE:${t.owner}.${p}`)} /> : <div className="ws-empty"><p>This connection was closed.</p></div>
                ) : t.kind === "graph" ? (
                  connView[t.connId] ? <GraphPage api={api} conn={t.connId} owner={t.owner} focus={t.focus} onError={onError}
                    onOpen={(k, o, n) => openObject(t.connId, toKind(k), o, n)} /> : <div className="ws-empty"><p>This connection was closed.</p></div>
                ) : t.kind === "history" ? (
                  <HistoryPage api={api} conns={conns} onError={onError} active={t.id === activeId} canInsert={tabs.some((x) => x.kind === "sheet")}
                    onOpenSql={(sql, key) => { const c = conns.find((x) => x.key === key) ?? conns[0]; if (c) newSheet(c.id, c.schemas.find((s) => s.toLowerCase() === c.user.toLowerCase()) ?? c.schemas[0], sql, true); }}
                    onInsertSql={(sql) => { const sh = tabs.find((x): x is SheetTab => x.kind === "sheet" && x.id === (activeSheet?.id ?? lastSheetId.current)); if (sh) { setTabs((ts) => ts.map((x) => (x.id === sh.id ? { ...sh, sql: sh.sql.trim() ? `${sh.sql.trimEnd()}\n\n${sql}` : sql, touched: true } : x))); setActiveId(sh.id); } }}
                    onOpenWorksheet={(w: WorksheetRec) => {
                      if (tabs.some((x) => x.id === w.uuid)) { setActiveId(w.uuid); return; }
                      const c = conns.find((x) => x.key === w.conn_key);
                      setTabs((ts) => [...ts, { id: w.uuid, kind: "sheet", connId: c?.id ?? "", connKey: w.conn_key, connName: w.conn_name, owner: w.owner, sql: w.sql, title: w.title, touched: true }]);
                      saved.current.set(w.uuid, sig(w.sql, w.owner, w.conn_key));
                      setActiveId(w.uuid);
                    }} />
                ) : t.kind === "sheet" ? (
                  <Worksheet api={api} conns={conns} tab={t} getCache={getCache} onError={onError} onSchemaOpened={onSchemaOpened}
                    onPatch={(p) => setTabs((ts) => ts.map((x) => (x.id === t.id ? ({ ...x, ...p, ...("sql" in p ? { touched: true } : {}) } as Tab) : x)))}
                    onOpen={(c, k, o, n, parent) => openObject(c, k, o, n, parent)} onSandbox={openSandbox} />
                ) : connView[t.connId] ? (
                  <ObjectPage api={api} conn={connView[t.connId]} target={t.target} cache={getCache(t.connId)} onError={onError}
                    onOpen={(k, o, n, parent) => openObject(t.connId, k, o, n, parent)}
                    onWorksheet={(sql) => { const c = connOf(t.connId); if (c) newSheet(c.id, t.target.owner, sql); }}
                    onGraph={() => openGraph(t.connId, t.target.owner, `${t.target.kind}:${t.target.owner}.${t.target.name}`)}
                    onPackages={() => openPackages(t.connId, t.target.owner, t.target.name)} />
                ) : <div className="ws-empty"><p>This connection was closed.</p></div>}
              </div>
            ))}
          </div>
        </section>
      </div>
      <footer className="statusbar">
        <span>{conns.length} connection{conns.length === 1 ? "" : "s"}</span>
        {conns.some((c) => c.profile.environment === "prod") && <span className="prod-note">PRODUCTION connected</span>}
        <span className="spacer" />
        <span className="muted">{notice}</span>
      </footer>
      {connectOpen && (
        <ConnectDialog
          api={api}
          profiles={profiles} error={connectError} busy={connecting} onConnect={connect}
          onSave={async (p) => { await api.saveProfile(p); setProfiles(await api.listProfiles()); }}
          onCancel={conns.length > 0 ? () => setConnectOpen(false) : undefined}
        />
      )}
      {importing && (() => { const c = connOf(activeConnId ?? "") ?? conns[0]; return c ? <ImportWizard api={api} conn={c.id} env={c.profile.environment} schemas={c.schemas} initial={{ owner: c.schemas.find((s) => s.toLowerCase() === c.user.toLowerCase()) ?? c.schemas[0], table: "" }} onClose={() => setImporting(false)} onError={onError} /> : null; })()}
      {aiOpen && <AiSettings api={api} onClose={() => setAiOpen(false)} onChanged={setAiStatus} />}
      {searchOpen && conns.length > 0 && <SearchPalette api={api} conns={conns} activeConnId={activeConnId} onPick={pick} onPickHistory={pickHistory} onClose={() => setSearchOpen(false)} />}
    </div>
  );
}
