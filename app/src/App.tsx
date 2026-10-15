import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { apiPromise, isDemo, type Api } from "./api";
import { InsightCache } from "./badges";
import { ConnectDialog } from "./components/Dialogs";
import { Explorer, type ConnState } from "./components/Explorer";
import { ObjectPage, type ObjTarget } from "./components/ObjectPage";
import { SearchPalette, type ConnHit } from "./components/SearchPalette";
import { Worksheet, type SheetTab } from "./components/Worksheet";
import { errText } from "./hooks";
import { Icon, KindIcon } from "./ui-icons";
import type { ObjectEntry, ObjectKind, Profile } from "./types";

interface ObjTab { id: string; kind: "object"; connId: string; target: ObjTarget }
type Tab = SheetTab | ObjTab;

const sameObj = (a: ObjTarget, b: ObjTarget) => a.owner === b.owner && a.kind === b.kind && a.name === b.name;
const toKind = (t: string): ObjectKind => (t.replace(" BODY", "") as ObjectKind);

export default function App() {
  const [api, setApi] = useState<Api>();
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

  useEffect(() => { apiPromise.then(async (a) => { setApi(a); setProfiles(await a.listProfiles()); setConnectOpen(true); }); }, []);

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

  const newSheet = useCallback((connId: string, owner: string, sql = "SELECT * FROM ") => {
    const id = `t${++seq.current}`;
    const n = tabs.filter((t) => t.kind === "sheet").length + 1;
    setTabs((t) => [...t, { id, kind: "sheet", connId, owner, sql, title: `SQL ${n}` }]);
    setActiveId(id);
  }, [tabs]);

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
      const id = await api.connect(p, password);
      const schemas = await api.listSchemas(id);
      const c: ConnState = { id, profile: p, schemas: schemas.length ? schemas : [p.user.toUpperCase()], user: p.user };
      setConns((cs) => [...cs, c]);
      setConnectOpen(false);
      const own = c.schemas.find((s) => s.toLowerCase() === p.user.toLowerCase()) ?? c.schemas[0];
      const tid = `t${++seq.current}`;
      setTabs((ts) => [...ts, { id: tid, kind: "sheet", connId: id, owner: own, sql: `SELECT * FROM ${own}.`, title: `SQL ${ts.filter((t) => t.kind === "sheet").length + 1}` }]);
      setActiveId(tid);
    } catch (e) { setConnectError(errText(e)); }
    finally { setConnecting(false); }
  }, [api]);

  const disconnect = useCallback(async (connId: string) => {
    if (!api) return;
    await api.disconnect(connId).catch(onError);
    caches.current.delete(connId);
    setConns((cs) => cs.filter((c) => c.id !== connId));
    setTabs((ts) => { const rest = ts.filter((t) => t.connId !== connId || t.kind === "sheet"); return rest; });
  }, [api, onError]);

  const closeTab = (id: string) => {
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

  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setSearchOpen(true); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  const active = tabs.find((t) => t.id === activeId);
  const activeObj = active?.kind === "object" ? active : undefined;
  const connOf = (id: string) => conns.find((c) => c.id === id);
  const connView = useMemo(() => Object.fromEntries(conns.map((c) => [c.id, { id: c.id, name: c.profile.name, env: c.profile.environment, user: c.user }])), [conns]);

  if (!api) return null;
  const activeConnId = active?.connId ?? conns[0]?.id;

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand"><span className="logo"><Icon name="database" size={16} /></span> Ora IDE</div>
        <button className="search-pill" onClick={() => setSearchOpen(true)} disabled={conns.length === 0}>
          <Icon name="search" size={14} /> <span>Search tables, columns, source…</span> <kbd>Ctrl K</kbd>
        </button>
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
              return (
                <div key={t.id} role="tab" aria-selected={t.id === activeId} className={`tab ${t.id === activeId ? "on" : ""}`} onClick={() => setActiveId(t.id)} title={c?.profile.name}>
                  <span className={`tab-stripe env-${env}`} />
                  {t.kind === "sheet" ? <Icon name="sql" size={14} /> : <KindIcon kind={t.target.kind} size={14} />}
                  <span className="tab-title">{t.kind === "sheet" ? t.title : t.target.name}</span>
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
                {t.kind === "sheet" ? (
                  <Worksheet api={api} conns={conns} tab={t} getCache={getCache} onError={onError} onSchemaOpened={onSchemaOpened}
                    onPatch={(p) => setTabs((ts) => ts.map((x) => (x.id === t.id ? ({ ...x, ...p } as Tab) : x)))}
                    onOpen={(c, k, o, n, parent) => openObject(c, k, o, n, parent)} />
                ) : connView[t.connId] ? (
                  <ObjectPage api={api} conn={connView[t.connId]} target={t.target} cache={getCache(t.connId)} onError={onError}
                    onOpen={(k, o, n, parent) => openObject(t.connId, k, o, n, parent)}
                    onWorksheet={(sql) => { const c = connOf(t.connId); if (c) newSheet(c.id, t.target.owner, sql); }} />
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
          profiles={profiles} error={connectError} busy={connecting} onConnect={connect}
          onSave={async (p) => { await api.saveProfile(p); setProfiles(await api.listProfiles()); }}
          onCancel={conns.length > 0 ? () => setConnectOpen(false) : undefined}
        />
      )}
      {searchOpen && conns.length > 0 && <SearchPalette api={api} conns={conns} activeConnId={activeConnId} onPick={pick} onClose={() => setSearchOpen(false)} />}
    </div>
  );
}
