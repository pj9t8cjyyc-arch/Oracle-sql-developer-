import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Api } from "../api";
import { Icon, KindIcon } from "../ui-icons";
import { KINDS, type ObjectEntry, type ObjectKind, type Profile } from "../types";

export interface ConnState { id: string; profile: Profile; schemas: string[]; user: string }
const LIMIT = 300;

interface Props {
  api: Api;
  conns: ConnState[];
  active?: { connId: string; owner: string; kind: ObjectKind; name: string };
  onOpen(connId: string, owner: string, kind: ObjectKind, entry: ObjectEntry): void;
  onNewSheet(connId: string, owner: string): void;
  onDisconnect(connId: string): void;
  onAdd(): void;
  onSchemaOpened(connId: string, owner: string): void;
  onError(e: unknown): void;
}

function Row({ depth, caret, icon, label, hint, right, active, bad, onClick, title }: {
  depth: number; caret?: "open" | "closed"; icon?: React.ReactNode; label: React.ReactNode; hint?: string; right?: React.ReactNode;
  active?: boolean; bad?: boolean; onClick?: () => void; title?: string;
}) {
  return (
    <div className={`trow ${active ? "active" : ""} ${bad ? "bad" : ""}`} style={{ paddingLeft: 8 + depth * 14 }} onClick={onClick} title={title} role="treeitem" aria-expanded={caret ? caret === "open" : undefined}>
      <span className="caret">{caret && <Icon name={caret === "open" ? "down" : "right"} size={12} />}</span>
      {icon}
      <span className="tlabel">{label}</span>
      {hint && <span className="thint">{hint}</span>}
      {right && <span className="tright">{right}</span>}
    </div>
  );
}

export function Explorer({ api, conns, active, onOpen, onNewSheet, onDisconnect, onAdd, onSchemaOpened, onError }: Props) {
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [counts, setCounts] = useState<Record<string, Record<string, number>>>({});
  const [lists, setLists] = useState<Record<string, ObjectEntry[] | "loading">>({});
  const [filter, setFilter] = useState<Record<string, string>>({});
  const asked = useRef(new Set<string>());

  const toggle = (k: string) => setOpen((s) => { const n = new Set(s); n.has(k) ? n.delete(k) : n.add(k); return n; });

  const loadCounts = useCallback((c: string, owner: string, force = false) => {
    const k = `${c}/${owner}`;
    if (!force && asked.current.has(k)) return;
    asked.current.add(k);
    api.objectCounts(c, owner).then((m) => setCounts((p) => ({ ...p, [k]: m }))).catch((e) => { asked.current.delete(k); onError(e); });
  }, [api, onError]);

  const loadList = useCallback((c: string, owner: string, kind: string, force = false) => {
    const k = `${c}/${owner}/${kind}`;
    if (!force && asked.current.has(k)) return;
    asked.current.add(k);
    setLists((p) => ({ ...p, [k]: "loading" }));
    api.listObjects(c, owner, kind).then((l) => setLists((p) => ({ ...p, [k]: l }))).catch((e) => { asked.current.delete(k); setLists((p) => { const { [k]: _x, ...r } = p; return r; }); onError(e); });
  }, [api, onError]);

  // Open each new connection and its own schema automatically.
  useEffect(() => {
    for (const c of conns) {
      const ck = c.id, sk = `${c.id}/${c.user}`;
      if (!asked.current.has(`init/${ck}`)) {
        asked.current.add(`init/${ck}`);
        const own = c.schemas.find((s) => s.toLowerCase() === c.user.toLowerCase()) ?? c.schemas[0];
        setOpen((s) => new Set(s).add(ck).add(own ? `${ck}/${own}` : sk));
        if (own) { loadCounts(c.id, own); onSchemaOpened(c.id, own); }
      }
    }
  }, [conns, loadCounts, onSchemaOpened]);

  const refresh = (c: ConnState) => {
    for (const k of [...asked.current]) if (k.startsWith(`${c.id}/`)) asked.current.delete(k);
    setLists((p) => Object.fromEntries(Object.entries(p).filter(([k]) => !k.startsWith(`${c.id}/`))));
    setCounts((p) => Object.fromEntries(Object.entries(p).filter(([k]) => !k.startsWith(`${c.id}/`))));
    for (const k of open) {
      const [id, owner, kind] = k.split("/");
      if (id !== c.id || !owner) continue;
      if (kind) loadList(id, owner, kind, true); else loadCounts(id, owner, true);
    }
  };

  const sorted = useMemo(() => conns.map((c) => ({ c, schemas: [...c.schemas].sort((a, b) => (a.toLowerCase() === c.user.toLowerCase() ? -1 : b.toLowerCase() === c.user.toLowerCase() ? 1 : a.localeCompare(b))) })), [conns]);

  return (
    <aside className="explorer" aria-label="Explorer">
      <div className="explorer-head">
        <span>Connections</span>
        <button className="ghost icon" title="New connection" onClick={onAdd}><Icon name="plus" size={15} /></button>
      </div>
      <div className="explorer-scroll" role="tree">
        {conns.length === 0 && (
          <div className="explorer-empty">
            <Icon name="database" size={30} />
            <p>No database connected yet.</p>
            <button className="primary" onClick={onAdd}>Connect to Oracle</button>
          </div>
        )}
        {sorted.map(({ c, schemas }) => {
          const ck = c.id;
          const f = (filter[ck] ?? "").toLowerCase();
          return (
            <div key={ck} className="conn-node">
              <Row
                depth={0} caret={open.has(ck) ? "open" : "closed"} onClick={() => toggle(ck)} title={`${c.profile.user}@${c.profile.host}:${c.profile.port}/${c.profile.service}`}
                icon={<span className={`env-dot env-${c.profile.environment}`} title={c.profile.environment} />}
                label={<><b>{c.profile.name}</b></>} hint={`${c.profile.user}@${c.profile.service}`}
                right={
                  <span className="row-actions" onClick={(e) => e.stopPropagation()}>
                    <button className="ghost icon" title="New SQL worksheet" onClick={() => onNewSheet(ck, c.schemas.find((s) => s.toLowerCase() === c.user.toLowerCase()) ?? c.schemas[0] ?? c.user)}><Icon name="sql" size={14} /></button>
                    <button className="ghost icon" title="Refresh" onClick={() => refresh(c)}><Icon name="refresh" size={14} /></button>
                    <button className="ghost icon" title="Disconnect" onClick={() => onDisconnect(ck)}><Icon name="plug" size={14} /></button>
                  </span>
                }
              />
              {open.has(ck) && (
                <>
                  <div className="tfilter" style={{ paddingLeft: 30 }}>
                    <Icon name="search" size={12} />
                    <input placeholder="Filter objects…" value={filter[ck] ?? ""} onChange={(e) => setFilter((p) => ({ ...p, [ck]: e.target.value }))} />
                  </div>
                  {schemas.map((owner) => {
                    const sk = `${ck}/${owner}`;
                    const cnt = counts[sk];
                    return (
                      <div key={sk}>
                        <Row depth={1} caret={open.has(sk) ? "open" : "closed"} icon={<Icon name="database" size={14} />} label={owner}
                          hint={owner.toLowerCase() === c.user.toLowerCase() ? "you" : undefined}
                          onClick={() => { toggle(sk); if (!open.has(sk)) { loadCounts(ck, owner); onSchemaOpened(ck, owner); } }} />
                        {open.has(sk) && !cnt && <Row depth={2} label={<span className="muted">loading…</span>} />}
                        {open.has(sk) && cnt && KINDS.filter(([k]) => cnt[k]).map(([kind, label]) => {
                          const kk = `${sk}/${kind}`;
                          const list = lists[kk];
                          const shown = Array.isArray(list) ? list.filter((e) => !f || e.name.toLowerCase().includes(f) || (e.parent ?? "").toLowerCase().includes(f)) : [];
                          return (
                            <div key={kk}>
                              <Row depth={2} caret={open.has(kk) ? "open" : "closed"} icon={<KindIcon kind={kind} size={14} />} label={label} right={<span className="count">{cnt[kind]}</span>}
                                onClick={() => { toggle(kk); if (!open.has(kk)) loadList(ck, owner, kind); }} />
                              {open.has(kk) && list === "loading" && <Row depth={3} label={<span className="muted">loading…</span>} />}
                              {open.has(kk) && Array.isArray(list) && shown.slice(0, LIMIT).map((e) => (
                                <Row key={e.name} depth={3} bad={!e.ok} title={e.detail ?? e.status}
                                  active={active?.connId === ck && active.owner === owner && active.kind === kind && active.name === e.name}
                                  label={e.name} hint={e.parent && (kind === "INDEX" || kind === "TRIGGER") ? e.parent : undefined}
                                  right={!e.ok ? <span className="status-tag">{e.status.toLowerCase()}</span> : undefined}
                                  onClick={() => onOpen(ck, owner, kind, e)} />
                              ))}
                              {open.has(kk) && Array.isArray(list) && shown.length > LIMIT && <Row depth={3} label={<span className="muted">{shown.length - LIMIT} more — refine the filter</span>} />}
                              {open.has(kk) && Array.isArray(list) && shown.length === 0 && <Row depth={3} label={<span className="muted">no match</span>} />}
                            </div>
                          );
                        })}
                      </div>
                    );
                  })}
                </>
              )}
            </div>
          );
        })}
      </div>
    </aside>
  );
}
