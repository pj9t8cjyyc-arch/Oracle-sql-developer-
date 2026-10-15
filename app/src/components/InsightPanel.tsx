import { useEffect, useState } from "react";
import type { Api } from "../api";
import { columnBadges, type InsightCache } from "../badges";
import type { ColumnInsight, Environment, TableInsight } from "../types";
import { SqlConfirmDialog } from "./Dialogs";

export interface Target { owner: string; table: string; column?: string }

const REASON: Record<string, string> = {
  update_of: "fires when this column is updated (UPDATE OF)",
  referenced: "uses this column (:NEW / :OLD)",
  any_update: "fires on any UPDATE of the row",
};

function CommentEditor({ api, env, owner, table, column, current, onSaved, onError }: {
  api: Api; env: Environment; owner: string; table: string; column: string | null; current: string | null;
  onSaved(): void; onError(e: unknown): void;
}) {
  const [text, setText] = useState(current ?? "");
  const [sql, setSql] = useState<string>();
  const [busy, setBusy] = useState(false);
  useEffect(() => setText(current ?? ""), [current, table, column]);
  const dirty = text !== (current ?? "");
  return (
    <div className="comment-editor">
      <textarea value={text} placeholder="Add a comment…" maxLength={4000} onChange={(e) => setText(e.target.value)} />
      <div className="actions">
        <button disabled={!dirty} onClick={() => setText(current ?? "")}>Reset</button>
        <button className="primary" disabled={!dirty} onClick={() => api.previewComment(owner, table, column, text).then(setSql).catch(onError)}>Save comment…</button>
      </div>
      {sql && (
        <SqlConfirmDialog
          title="Save comment" sql={sql} busy={busy} prod={env === "prod"}
          note="COMMENT is DDL: it takes effect immediately and cannot be rolled back."
          onCancel={() => setSql(undefined)}
          onRun={async () => {
            setBusy(true);
            try { await api.saveComment(owner, table, column, text); setSql(undefined); onSaved(); } catch (e) { onError(e); setSql(undefined); }
            finally { setBusy(false); }
          }}
        />
      )}
    </div>
  );
}

function ColumnCard({ c, api, env, owner, table, onSaved, onError }: { c: ColumnInsight; api: Api; env: Environment; owner: string; table: string; onSaved(): void; onError(e: unknown): void }) {
  const fmt = (n: number | null) => (n === null ? "—" : n.toLocaleString());
  return (
    <div className="card">
      <h4>{c.name} <span className="muted">{c.data_type}{c.nullable ? "" : " NOT NULL"}</span></h4>
      <div className="badges">{columnBadges(c).map((b, i) => <span key={i} className={`badge ${b.tone}`} title={b.title}>{b.text}</span>)}</div>
      <dl>
        <dt>Distinct / nulls</dt><dd>{fmt(c.distinct)} / {fmt(c.nulls)}{c.histogram ? ` · ${c.histogram.toLowerCase()} histogram` : ""}</dd>
        {c.foreign_keys.map((f) => <><dt>Foreign key</dt><dd key={f.constraint}>→ {f.ref_owner}.{f.ref_table}.{f.ref_column} <span className="muted">({f.constraint})</span></dd></>)}
        {c.checks.length > 0 && <><dt>Check</dt><dd><code>{c.checks.join(" AND ")}</code></dd></>}
        <dt>Indexes</dt>
        <dd>{c.indexes.length === 0 ? <span className="warn">none — filtering on this column scans the table</span> : c.indexes.map((i) => (
          <div key={i.name}>{i.name} <span className="muted">pos {i.position}{i.leading ? " · leading" : " · not leading"}{i.unique ? " · unique" : ""}</span>{!i.valid && <span className="warn"> · UNUSABLE</span>}</div>
        ))}</dd>
        <dt>Triggers</dt>
        <dd>{c.triggers.length === 0 ? <span className="muted">none</span> : c.triggers.map((t) => (
          <div key={t.name}>⚡ {t.name} <span className="muted">{t.timing} {t.event}{t.enabled ? "" : " · DISABLED"}</span><div className="muted small">{REASON[t.reason]}</div></div>
        ))}</dd>
      </dl>
      <CommentEditor api={api} env={env} owner={owner} table={table} column={c.name} current={c.comment} onSaved={onSaved} onError={onError} />
    </div>
  );
}

export function InsightPanel({ api, cache, env, target, onSelect, onSaved, onError, onClose }: {
  api: Api; cache: InsightCache; env: Environment; target: Target;
  onSelect(t: Target): void; onSaved(t: Target): void; onError(e: unknown): void; onClose(): void;
}) {
  const [data, setData] = useState<TableInsight>();
  const [rev, setRev] = useState(0);
  useEffect(() => {
    let live = true;
    setData(undefined);
    cache.get(target.owner, target.table).then((d) => live && setData(d)).catch((e) => live && onError(e));
    return () => { live = false; };
  }, [cache, target.owner, target.table, rev, onError]);

  const col = data?.columns.find((c) => c.name === target.column);
  const saved = () => { cache.invalidate(target.owner, target.table); setRev((r) => r + 1); onSaved(target); };

  return (
    <aside className="insight">
      <div className="insight-head">
        <strong>{target.owner}.{target.table}</strong>
        <button onClick={onClose} aria-label="Close insight panel">✕</button>
      </div>
      {!data ? <div className="muted pad">Loading…</div> : (
        <div className="insight-scroll">
          {!col && (
            <>
              <h4>Table comment</h4>
              <CommentEditor api={api} env={env} owner={target.owner} table={target.table} column={null} current={data.comment} onSaved={saved} onError={onError} />
            </>
          )}
          {col && <ColumnCard c={col} api={api} env={env} owner={target.owner} table={target.table} onSaved={saved} onError={onError} />}
          <h4>Columns</h4>
          <div className="collist">
            {data.columns.map((c) => (
              <div key={c.name} className={`colrow ${c.name === target.column ? "sel" : ""}`} onClick={() => onSelect({ ...target, column: c.name })}>
                <span>{c.name}</span>
                <span className="badges">{columnBadges(c).map((b, i) => <span key={i} className={`badge ${b.tone}`} title={b.title}>{b.text}</span>)}</span>
              </div>
            ))}
          </div>
          {data.indexes.length > 0 && <><h4>Indexes</h4>{data.indexes.map((i) => <div key={i.name} className="line">{i.name}{i.unique ? " (unique)" : ""}{!i.valid && <span className="warn"> UNUSABLE</span>} <span className="muted">({i.columns.join(", ")})</span></div>)}</>}
          {data.triggers.length > 0 && <><h4>Triggers</h4>{data.triggers.map((t) => <div key={t.name} className="line">⚡ {t.name}{!t.enabled && <span className="warn"> DISABLED</span>} <span className="muted">{t.timing} {t.event}</span></div>)}</>}
          {data.referenced_by.length > 0 && <><h4>Referenced by (foreign keys)</h4>{data.referenced_by.map((r) => <div key={r.constraint} className="line link" onClick={() => onSelect({ owner: r.owner, table: r.table })}>{r.owner}.{r.table} <span className="muted">{r.constraint}</span></div>)}</>}
          {(data.uses.length > 0 || data.used_by.length > 0) && <h4>Dependencies</h4>}
          {data.uses.map((d) => <div key={"u" + d.name} className="line link" onClick={() => d.object_type === "TABLE" && onSelect({ owner: d.owner, table: d.name })}>uses {d.object_type.toLowerCase()} {d.owner}.{d.name}</div>)}
          {data.used_by.map((d) => <div key={"b" + d.name} className="line">used by {d.object_type.toLowerCase()} {d.owner}.{d.name}</div>)}
        </div>
      )}
    </aside>
  );
}
