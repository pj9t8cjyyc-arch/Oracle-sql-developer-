import { createContext, useContext, useEffect, useMemo, useState } from "react";
import type { Api } from "../api";
import { columnBadges, type InsightCache } from "../badges";
import { useAsync } from "../hooks";
import { Icon, KindIcon } from "../ui-icons";
import type { ColumnInsight, Environment, ObjectEntry, ObjectKind, TableInsight } from "../types";
import { CodeView } from "./CodeView";
import { ColumnFocus, CommentEditor } from "./ColumnFocus";
import { ExportDialog } from "./ExportDialog";
import { ImportWizard } from "./ImportWizard";
import { KnowledgeDialog } from "./KnowledgeDialog";
import { ConstraintRows, Empty, IndexRows, ReferencedByRows, SequenceRows, TriggerRows, type OpenFn } from "./Related";
import { ResultGrid } from "./ResultGrid";

export interface ObjTarget {
  owner: string; kind: ObjectKind; name: string;
  /** Table an index/trigger belongs to, when known. */
  parent?: string | null;
  entry?: ObjectEntry | null;
  column?: string;
}
export interface ConnView { id: string; name: string; env: Environment; user: string }

interface PageProps {
  api: Api; conn: ConnView; target: ObjTarget; cache: InsightCache;
  onOpen: OpenFn; onWorksheet(sql: string): void; onError(e: unknown): void;
  onGraph?(): void; onPackages?(): void;
}

const LinkCtx = createContext<{ graph?(): void; packages?(): void; knowledge?(): void }>({});

const toKind = (t: string): ObjectKind | null => {
  const k = t.replace(" BODY", "") as ObjectKind;
  return (["TABLE", "VIEW", "MATERIALIZED VIEW", "INDEX", "SEQUENCE", "TRIGGER", "PROCEDURE", "FUNCTION", "PACKAGE", "TYPE", "SYNONYM"] as string[]).includes(k) ? k : null;
};

function Segmented<T extends string>({ tabs, value, onChange }: { tabs: [T, string, number?][]; value: T; onChange(t: T): void }) {
  return (
    <div className="segmented" role="tablist">
      {tabs.map(([id, label, n]) => (
        <button key={id} role="tab" aria-selected={value === id} className={value === id ? "on" : ""} onClick={() => onChange(id)}>
          {label}{n !== undefined && <span className="count">{n}</span>}
        </button>
      ))}
    </div>
  );
}

function Header({ target, conn, status, children }: { target: ObjTarget; conn: ConnView; status?: { ok: boolean; text: string }; children?: React.ReactNode }) {
  const links = useContext(LinkCtx);
  return (
    <header className="obj-head">
      <KindIcon kind={target.kind} size={22} />
      <div className="obj-title">
        <h2>{target.owner}.<b>{target.name}</b></h2>
        <div className="obj-meta">
          <span className="chip">{target.kind.toLowerCase()}</span>
          <span className={`chip env-${conn.env}`}><Icon name="database" size={11} /> {conn.name}</span>
          {status && <span className={`chip ${status.ok ? "ok" : "bad"}`}>{status.text.toLowerCase()}</span>}
        </div>
      </div>
      <div className="obj-actions">
        {children}
        {target.kind === "PACKAGE" && links.packages && <button onClick={links.packages} title="Members, callers and tables used"><Icon name="package" size={14} /> Members &amp; usage</button>}
        {target.kind !== "INDEX" && links.knowledge && <button onClick={links.knowledge} title="Download this object and everything linked to it as markdown (columns, indexes, triggers, packages, SQL)"><Icon name="download" size={14} /> Knowledge .md</button>}
        {target.kind !== "INDEX" && links.graph && <button onClick={links.graph} title="What it uses and what uses it"><Icon name="out" size={14} /> Graph</button>}
      </div>
    </header>
  );
}

function Card({ title, count, onMore, children }: { title: string; count?: number; onMore?(): void; children: React.ReactNode }) {
  return (
    <section className="card">
      <div className="card-head"><h3>{title}{count !== undefined && <span className="count">{count}</span>}</h3>{onMore && <button className="link" onClick={onMore}>View all</button>}</div>
      <div className="card-body">{children}</div>
    </section>
  );
}

function Load({ state, children }: { state: { loading: boolean; error: string }; children: React.ReactNode }) {
  if (state.error) return <div className="error pad">{state.error}</div>;
  if (state.loading) return <div className="muted pad">Loading…</div>;
  return <>{children}</>;
}

// ------------------------------------------------------------------ tables
type TableTab = "overview" | "columns" | "indexes" | "constraints" | "triggers" | "sequences" | "relations" | "data" | "ddl";

function ColumnsTable({ cols, selected, onSelect }: { cols: ColumnInsight[]; selected?: string; onSelect(c: string): void }) {
  const [q, setQ] = useState("");
  const shown = cols.filter((c) => c.name.toLowerCase().includes(q.toLowerCase()) || (c.comment ?? "").toLowerCase().includes(q.toLowerCase()));
  return (
    <div className="cols-pane">
      <input className="filter" placeholder="Filter columns or comments…" value={q} onChange={(e) => setQ(e.target.value)} />
      <table className="dict">
        <thead><tr><th>#</th><th>Column</th><th>Type</th><th>Null</th><th>Distinct</th><th>Comment</th></tr></thead>
        <tbody>
          {shown.map((c) => (
            <tr key={c.name} className={c.name === selected ? "sel" : ""} onClick={() => onSelect(c.name)}>
              <td className="muted">{cols.indexOf(c) + 1}</td>
              <td><span className="cname">{c.name}</span> <span className="badges">{columnBadges(c).filter((b) => b.text !== "✎").map((b, i) => <span key={i} className={`badge ${b.tone}`} title={b.title}>{b.text}</span>)}</span></td>
              <td className="mono">{c.data_type}</td>
              <td>{c.nullable ? "" : "NOT NULL"}</td>
              <td className="num">{c.distinct === null ? "—" : c.distinct.toLocaleString()}</td>
              <td className="muted ellipsis" title={c.comment ?? ""}>{c.comment ?? ""}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {shown.length === 0 && <Empty>No column matches.</Empty>}
    </div>
  );
}

function DataTab({ api, conn, t }: { api: Api; conn: string; t: TableInsight }) {
  const res = useAsync(() => api.previewTable(conn, t.owner, t.table, 100), [api, conn, t.owner, t.table]);
  const badges = useMemo(() => Object.fromEntries(t.columns.map((c) => [c.name, columnBadges(c)])), [t]);
  return (
    <Load state={res}>
      {res.data?.kind === "rows" ? (
        <div className="data-pane"><ResultGrid columns={res.data.columns} rows={res.data.rows} onNearEnd={() => {}} badges={badges} onSelectColumn={() => {}} />
          <div className="muted small data-foot">First {res.data.rows.length} rows · open a worksheet for more</div></div>
      ) : <Empty>No rows to show.</Empty>}
    </Load>
  );
}

function DdlTab({ api, conn, target }: { api: Api; conn: string; target: ObjTarget }) {
  const res = useAsync(() => api.objectDdl(conn, target.owner, target.name, target.kind), [api, conn, target.owner, target.name, target.kind]);
  return <Load state={res}><CodeView value={res.data ?? ""} empty="No DDL returned (DBMS_METADATA may need SELECT_CATALOG_ROLE)." /></Load>;
}

function DepsTab({ api, conn, target, onOpen }: { api: Api; conn: string; target: ObjTarget; onOpen: OpenFn }) {
  const res = useAsync(() => api.objectDependencies(conn, target.owner, target.name), [api, conn, target.owner, target.name]);
  const list = (title: string, items: { owner: string; name: string; object_type: string }[]) => (
    <Card title={title} count={items.length}>
      {items.length === 0 ? <Empty>None.</Empty> : items.map((d) => {
        const k = toKind(d.object_type);
        return (
          <div key={`${d.owner}${d.name}${d.object_type}`} className={`rel-item ${k ? "clickable" : ""}`} onClick={() => k && onOpen(k, d.owner, d.name)}>
            {k ? <KindIcon kind={k} size={15} /> : <span />}
            <div className="rel-main"><div className="rel-title"><span className="rel-name">{d.owner}.{d.name}</span><span className="tag muted">{d.object_type.toLowerCase()}</span></div></div>
          </div>
        );
      })}
    </Card>
  );
  return <Load state={res}><div className="grid2">{list("Uses", res.data?.uses ?? [])}{list("Used by", res.data?.used_by ?? [])}</div></Load>;
}

function TableView({ api, conn, target, cache, onOpen, onWorksheet, onError }: PageProps) {
  const [tab, setTab] = useState<TableTab>("overview");
  const [col, setCol] = useState<string | undefined>(target.column);
  const [xfer, setXfer] = useState<"export" | "import">();
  useEffect(() => { if (target.column) { setCol(target.column); setTab("columns"); } }, [target.column, target.name]);
  const res = useAsync(() => cache.get(target.owner, target.name), [cache, target.owner, target.name]);
  const t = res.data;
  const isTable = target.kind !== "VIEW";
  const saved = () => { cache.invalidate(target.owner, target.name); res.reload(); };
  const all: [TableTab, string, number?][] = t
    ? [
        ["overview", "Overview"], ["columns", "Columns", t.columns.length],
        ...(isTable ? ([["indexes", "Indexes", t.indexes.length], ["constraints", "Constraints"], ["triggers", "Triggers", t.triggers.length], ["sequences", "Sequences", t.sequences.length]] as [TableTab, string, number?][]) : []),
        ["relations", "Relations"], ["data", "Data"], ["ddl", "DDL"],
      ]
    : [];
  const fkCount = t ? t.columns.reduce((n, c) => n + c.foreign_keys.length, 0) : 0;

  return (
    <>
      <Header target={target} conn={conn} status={target.entry ? { ok: target.entry.ok, text: target.entry.status } : undefined}>
        <button onClick={() => onWorksheet(`SELECT * FROM ${target.owner}.${target.name}`)}><Icon name="sql" size={14} /> Query</button>
        <button onClick={() => setXfer("export")} title="Export to CSV, Excel or SQL INSERT"><Icon name="download" size={14} /> Export</button>
        {target.kind === "TABLE" && <button onClick={() => setXfer("import")} title="Load a CSV or Excel file into this table"><Icon name="upload" size={14} /> Import</button>}
      </Header>
      {xfer === "export" && <ExportDialog api={api} conn={conn.id} env={conn.env} source={{ type: "table", owner: target.owner, table: target.name, where_clause: null }} label={`${target.owner}.${target.name}`} defaultName={target.name.toLowerCase()} onClose={() => setXfer(undefined)} onError={onError} />}
      {xfer === "import" && <ImportWizard api={api} conn={conn.id} env={conn.env} schemas={[target.owner]} initial={{ owner: target.owner, table: target.name }} onClose={() => setXfer(undefined)} onError={onError} onImported={() => { cache.invalidate(target.owner, target.name); res.reload(); }} />}
      <Load state={res}>
        {t && (
          <>
            <Segmented<TableTab> tabs={all} value={tab} onChange={setTab} />
            <div className="obj-body">
              {tab === "overview" && (
                <div className="overview">
                  <div className="stats">
                    {isTable && ([["indexes", "Indexes", t.indexes.length], ["triggers", "Triggers", t.triggers.length], ["sequences", "Sequences", t.sequences.length]] as [TableTab, string, number][]).map(([id, l, n]) => (
                      <button key={id} className="stat" onClick={() => setTab(id)}><b>{n}</b>{l}</button>
                    ))}
                    <button className="stat" onClick={() => setTab("columns")}><b>{t.columns.length}</b>Columns</button>
                    {isTable && <button className="stat" onClick={() => setTab("relations")}><b>{fkCount}</b>Foreign keys</button>}
                    <button className="stat" onClick={() => setTab("relations")}><b>{t.referenced_by.length}</b>Referenced by</button>
                  </div>
                  <div className="grid2">
                    {isTable && <Card title="Indexes" count={t.indexes.length} onMore={() => setTab("indexes")}><IndexRows t={t} onOpen={onOpen} /></Card>}
                    {isTable && <Card title="Triggers" count={t.triggers.length} onMore={() => setTab("triggers")}><TriggerRows t={t} onOpen={onOpen} /></Card>}
                    {isTable && <Card title="Sequences" count={t.sequences.length} onMore={() => setTab("sequences")}><SequenceRows t={t} onOpen={onOpen} /></Card>}
                    {isTable && <Card title="Constraints" onMore={() => setTab("constraints")}><ConstraintRows t={t} onOpen={onOpen} /></Card>}
                    <Card title="Referenced by" count={t.referenced_by.length}><ReferencedByRows t={t} onOpen={onOpen} /></Card>
                    <Card title="Table comment">
                      <CommentEditor api={api} conn={conn.id} env={conn.env} owner={t.owner} table={t.table} column={null} current={t.comment} onSaved={saved} onError={onError} />
                    </Card>
                  </div>
                </div>
              )}
              {tab === "columns" && (
                <div className="split">
                  <ColumnsTable cols={t.columns} selected={col} onSelect={setCol} />
                  <div className="focus-pane">
                    {col ? <ColumnFocus api={api} conn={conn.id} env={conn.env} t={t} column={col} onOpen={onOpen} onSaved={saved} onError={onError} />
                      : <div className="focus-hint"><Icon name="columns" size={28} /><p>Select a column to see its indexes, triggers, sequences and constraints.</p></div>}
                  </div>
                </div>
              )}
              {tab === "indexes" && <div className="list-page"><IndexRows t={t} onOpen={onOpen} /></div>}
              {tab === "constraints" && <div className="list-page"><ConstraintRows t={t} onOpen={onOpen} /></div>}
              {tab === "triggers" && <div className="list-page"><TriggerRows t={t} onOpen={onOpen} /></div>}
              {tab === "sequences" && <div className="list-page"><SequenceRows t={t} onOpen={onOpen} /></div>}
              {tab === "relations" && (
                <div className="grid2">
                  <Card title="Foreign keys (this table → parent)">
                    <ConstraintRows t={{ ...t, columns: t.columns.map((c) => ({ ...c, checks: [], unique: false, primary_key: false, nullable: true })) }} onOpen={onOpen} />
                  </Card>
                  <Card title="Referenced by" count={t.referenced_by.length}><ReferencedByRows t={t} onOpen={onOpen} /></Card>
                  <Card title="Uses" count={t.uses.length}>{t.uses.length ? t.uses.map((d) => <div key={d.name} className="rel-item clickable" onClick={() => toKind(d.object_type) && onOpen(toKind(d.object_type)!, d.owner, d.name)}><div className="rel-main"><div className="rel-title"><span className="rel-name">{d.owner}.{d.name}</span><span className="tag muted">{d.object_type.toLowerCase()}</span></div></div></div>) : <Empty>None.</Empty>}</Card>
                  <Card title="Used by" count={t.used_by.length}>{t.used_by.length ? t.used_by.map((d) => <div key={d.name} className="rel-item clickable" onClick={() => toKind(d.object_type) && onOpen(toKind(d.object_type)!, d.owner, d.name)}><div className="rel-main"><div className="rel-title"><span className="rel-name">{d.owner}.{d.name}</span><span className="tag muted">{d.object_type.toLowerCase()}</span></div></div></div>) : <Empty>None.</Empty>}</Card>
                </div>
              )}
              {tab === "data" && <DataTab api={api} conn={conn.id} t={t} />}
              {tab === "ddl" && <DdlTab api={api} conn={conn.id} target={target} />}
            </div>
          </>
        )}
      </Load>
    </>
  );
}

// ----------------------------------------------------------- other objects
function IndexView({ api, conn, target, cache, onOpen, onWorksheet, onError }: PageProps) {
  const [tab, setTab] = useState<"overview" | "ddl">("overview");
  const parent = target.parent ?? target.entry?.parent ?? null;
  const res = useAsync(() => (parent ? cache.get(target.owner, parent) : Promise.resolve(undefined)), [cache, target.owner, parent]);
  const idx = res.data?.indexes.find((i) => i.name === target.name);
  return (
    <>
      <Header target={target} conn={conn} status={target.entry ? { ok: target.entry.ok, text: target.entry.status } : undefined}>
        {parent && <button onClick={() => onOpen("TABLE", target.owner, parent)}><Icon name="table" size={14} /> Open table</button>}
      </Header>
      <Segmented<"overview" | "ddl"> tabs={[["overview", "Overview"], ["ddl", "DDL"]]} value={tab} onChange={setTab} />
      <div className="obj-body">
        {tab === "ddl" ? <DdlTab api={api} conn={conn.id} target={target} /> : (
          <Load state={res}>
            {idx && res.data ? (
              <div className="grid2">
                <Card title="Definition">
                  <div className="kv"><span>Table</span><b><button className="link" onClick={() => onOpen("TABLE", target.owner, parent!)}>{target.owner}.{parent}</button></b></div>
                  <div className="kv"><span>Type</span><b>{idx.index_type.toLowerCase()}{idx.unique ? " · unique" : ""}</b></div>
                  <div className="kv"><span>Status</span><b>{idx.valid ? "valid" : "UNUSABLE — rebuild it"}</b></div>
                  <h4 className="sub">Columns (in index order)</h4>
                  {idx.columns.map((c, n) => <div key={c} className="kv"><span>{n + 1}</span><b>{c}</b></div>)}
                </Card>
                <Card title="Why it matters">
                  <p className="note">{idx.columns.length > 1 ? `Queries benefit most when they filter on ${idx.columns[0]} (the leading column), optionally followed by ${idx.columns.slice(1).join(", ")}.` : `Speeds up filters and joins on ${idx.columns[0]}.`}</p>
                  {!idx.valid && <p className="note warn">This index is unusable and ignored by the optimizer until rebuilt.</p>}
                  <button onClick={() => onWorksheet(`ALTER INDEX ${target.owner}.${target.name} REBUILD`)} disabled={idx.valid}>Draft rebuild statement</button>
                </Card>
              </div>
            ) : <Empty>{parent ? "Index details not found." : "Open this index from its table (or use the DDL tab) to see its columns."}</Empty>}
          </Load>
        )}
      </div>
    </>
  );
  void onError;
}

function TriggerView({ api, conn, target, cache, onOpen, onError }: PageProps) {
  const [tab, setTab] = useState<"overview" | "source" | "ddl" | "deps">("overview");
  const parent = target.parent ?? target.entry?.parent ?? null;
  const res = useAsync(() => (parent ? cache.get(target.owner, parent) : Promise.resolve(undefined)), [cache, target.owner, parent]);
  const info = res.data?.triggers.find((t) => t.name === target.name);
  const src = useAsync(() => (tab === "source" ? api.objectSource(conn.id, target.owner, target.name, "TRIGGER") : Promise.resolve("")), [api, conn.id, target.owner, target.name, tab]);
  void onError;
  return (
    <>
      <Header target={target} conn={conn} status={target.entry ? { ok: target.entry.ok, text: target.entry.status } : undefined}>
        {parent && <button onClick={() => onOpen("TABLE", target.owner, parent)}><Icon name="table" size={14} /> Open table</button>}
      </Header>
      <Segmented<"overview" | "source" | "ddl" | "deps"> tabs={[["overview", "Overview"], ["source", "Source"], ["ddl", "DDL"], ["deps", "Dependencies"]]} value={tab} onChange={setTab} />
      <div className="obj-body">
        {tab === "overview" && (
          <Load state={res}>
            <div className="grid2">
              <Card title="Definition">
                {parent && <div className="kv"><span>Table</span><b><button className="link" onClick={() => onOpen("TABLE", target.owner, parent)}>{target.owner}.{parent}</button></b></div>}
                <div className="kv"><span>Fires</span><b>{info ? `${info.timing} ${info.event}` : target.entry?.detail ?? "—"}</b></div>
                <div className="kv"><span>Status</span><b>{(info?.enabled ?? target.entry?.ok) ? "enabled" : "DISABLED"}</b></div>
                {info?.when_clause && <div className="kv"><span>WHEN</span><b className="mono">{info.when_clause}</b></div>}
                {info && info.columns.length > 0 && <div className="kv"><span>Columns</span><b>{info.columns.join(", ")}</b></div>}
              </Card>
              <Card title="Sequences used" count={info?.sequences.length ?? 0}>
                {info && info.sequences.length ? info.sequences.map((s) => (
                  <div key={s} className="rel-item clickable" onClick={() => onOpen("SEQUENCE", info.owner, s)}><KindIcon kind="SEQUENCE" size={15} /><div className="rel-main"><div className="rel-title"><span className="rel-name">{s}</span></div></div></div>
                )) : <Empty>No sequence usage detected in the trigger body.</Empty>}
              </Card>
            </div>
          </Load>
        )}
        {tab === "source" && <Load state={src}><CodeView value={src.data ?? ""} empty="No source (the trigger may be wrapped)." /></Load>}
        {tab === "ddl" && <DdlTab api={api} conn={conn.id} target={target} />}
        {tab === "deps" && <DepsTab api={api} conn={conn.id} target={target} onOpen={onOpen} />}
      </div>
    </>
  );
}

function SequenceView({ api, conn, target, onOpen }: PageProps) {
  const [tab, setTab] = useState<"overview" | "ddl" | "deps">("overview");
  const res = useAsync(() => api.sequenceInfo(conn.id, target.owner, target.name), [api, conn.id, target.owner, target.name]);
  const s = res.data;
  const big = (v: string) => (v.length > 12 ? "unbounded (" + v.slice(0, 3) + "…)" : v);
  return (
    <>
      <Header target={target} conn={conn} />
      <Segmented<"overview" | "ddl" | "deps"> tabs={[["overview", "Overview"], ["deps", "Used by"], ["ddl", "DDL"]]} value={tab} onChange={setTab} />
      <div className="obj-body">
        {tab === "overview" && (
          <Load state={res}>
            {s ? (
              <div className="grid2">
                <Card title="Properties">
                  <div className="kv"><span>Next value</span><b>{s.last_number}</b></div>
                  <div className="kv"><span>Increment</span><b>{s.increment}</b></div>
                  <div className="kv"><span>Min / max</span><b>{s.min} / {big(s.max)}</b></div>
                  <div className="kv"><span>Cache</span><b>{s.cache === "0" ? "none" : s.cache}</b></div>
                  <div className="kv"><span>Cycle · Order</span><b>{s.cycle ? "cycles" : "no cycle"} · {s.ordered ? "ordered" : "unordered"}</b></div>
                </Card>
                <Card title="Reading this">
                  <p className="note">“Next value” is what the database will hand out next. With a cache, values already reserved in memory can be skipped after a restart, so gaps are normal.</p>
                </Card>
              </div>
            ) : <Empty>Sequence not found or not visible to this user.</Empty>}
          </Load>
        )}
        {tab === "deps" && <DepsTab api={api} conn={conn.id} target={target} onOpen={onOpen} />}
        {tab === "ddl" && <DdlTab api={api} conn={conn.id} target={target} />}
      </div>
    </>
  );
}

function CodeObjectView({ api, conn, target, onOpen }: PageProps) {
  const hasBody = target.kind === "PACKAGE" || target.kind === "TYPE";
  const [tab, setTab] = useState<"spec" | "body" | "ddl" | "deps">("spec");
  const kindForTab = tab === "body" ? `${target.kind} BODY` : target.kind;
  const src = useAsync(() => (tab === "spec" || tab === "body" ? api.objectSource(conn.id, target.owner, target.name, kindForTab) : Promise.resolve("")), [api, conn.id, target.owner, target.name, tab]);
  const tabs: [typeof tab, string][] = [["spec", hasBody ? "Specification" : "Source"], ...(hasBody ? ([["body", "Body"]] as [typeof tab, string][]) : []), ["deps", "Dependencies"], ["ddl", "DDL"]];
  return (
    <>
      <Header target={target} conn={conn} status={target.entry ? { ok: target.entry.ok, text: target.entry.status } : undefined} />
      <Segmented<typeof tab> tabs={tabs} value={tab} onChange={setTab} />
      <div className="obj-body">
        {(tab === "spec" || tab === "body") && <Load state={src}><CodeView value={src.data ?? ""} empty="No source available (wrapped or not visible)." /></Load>}
        {tab === "deps" && <DepsTab api={api} conn={conn.id} target={target} onOpen={onOpen} />}
        {tab === "ddl" && <DdlTab api={api} conn={conn.id} target={target} />}
      </div>
    </>
  );
}

function SynonymView({ api, conn, target, onOpen }: PageProps) {
  const m = /→ (\w+)\.(\w+)/.exec(target.entry?.detail ?? "");
  return (
    <>
      <Header target={target} conn={conn} />
      <div className="obj-body">
        <div className="grid2">
          <Card title="Target">
            <div className="kv"><span>Points to</span><b>{target.entry?.detail?.replace("→ ", "") ?? "—"}</b></div>
            {m && <button onClick={() => onOpen("TABLE", m[1], m[2])}><Icon name="table" size={14} /> Open target</button>}
          </Card>
          <Card title="DDL"><DdlTab api={api} conn={conn.id} target={target} /></Card>
        </div>
      </div>
    </>
  );
}

export function ObjectPage(props: PageProps) {
  const { kind } = props.target;
  const [kb, setKb] = useState(false);
  const canKb = kind !== "INDEX";
  return (
    <LinkCtx.Provider value={{ graph: props.onGraph, packages: props.onPackages, knowledge: canKb ? () => setKb(true) : undefined }}>
      {kb && <KnowledgeDialog api={props.api} conn={props.conn.id} owner={props.target.owner} focus={`${kind}:${props.target.owner}.${props.target.name}`} label={`${props.target.owner}.${props.target.name}`} onClose={() => setKb(false)} onError={props.onError} />}
    <div className="object-page">
      {kind === "TABLE" || kind === "VIEW" || kind === "MATERIALIZED VIEW" ? <TableView {...props} />
        : kind === "INDEX" ? <IndexView {...props} />
        : kind === "TRIGGER" ? <TriggerView {...props} />
        : kind === "SEQUENCE" ? <SequenceView {...props} />
        : kind === "SYNONYM" ? <SynonymView {...props} />
        : <CodeObjectView {...props} />}
    </div>
    </LinkCtx.Provider>
  );
}
