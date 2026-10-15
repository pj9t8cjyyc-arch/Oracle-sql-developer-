import { useEffect, useState } from "react";
import type { Api } from "../api";
import { columnBadges } from "../badges";
import type { Environment, TableInsight } from "../types";
import { SqlConfirmDialog } from "./Dialogs";
import { ConstraintRows, IndexRows, SequenceRows, TriggerRows, type OpenFn } from "./Related";

export function CommentEditor({ api, conn, env, owner, table, column, current, onSaved, onError }: {
  api: Api; conn: string; env: Environment; owner: string; table: string; column: string | null; current: string | null;
  onSaved(): void; onError(e: unknown): void;
}) {
  const [text, setText] = useState(current ?? "");
  const [sql, setSql] = useState<string>();
  const [busy, setBusy] = useState(false);
  useEffect(() => setText(current ?? ""), [current, table, column]);
  const dirty = text !== (current ?? "");
  return (
    <div className="comment-editor">
      <textarea value={text} placeholder={column ? "Describe this column…" : "Describe this table…"} maxLength={4000} onChange={(e) => setText(e.target.value)} />
      {dirty && (
        <div className="actions">
          <button className="ghost" onClick={() => setText(current ?? "")}>Reset</button>
          <button className="primary" onClick={() => api.previewComment(owner, table, column, text).then(setSql).catch(onError)}>Save comment…</button>
        </div>
      )}
      {sql && (
        <SqlConfirmDialog
          title="Save comment" sql={sql} busy={busy} prod={env === "prod"}
          note="COMMENT is DDL: it takes effect immediately and cannot be rolled back."
          onCancel={() => setSql(undefined)}
          onRun={async () => {
            setBusy(true);
            try { await api.saveComment(conn, owner, table, column, text); setSql(undefined); onSaved(); } catch (e) { onError(e); setSql(undefined); }
            finally { setBusy(false); }
          }}
        />
      )}
    </div>
  );
}

function Section({ title, count, children }: { title: string; count?: number; children: React.ReactNode }) {
  return (
    <section className="focus-section">
      <h4>{title}{count !== undefined && <span className="count">{count}</span>}</h4>
      {children}
    </section>
  );
}

/** Everything that touches one column: constraints, indexes, triggers, sequences, stats, comment. */
export function ColumnFocus({ api, conn, env, t, column, onOpen, onSaved, onError, onClose }: {
  api: Api; conn: string; env: Environment; t: TableInsight; column: string;
  onOpen: OpenFn; onSaved(): void; onError(e: unknown): void; onClose?(): void;
}) {
  const c = t.columns.find((x) => x.name === column);
  if (!c) return <div className="empty-note">Column {column} not found.</div>;
  const fmt = (n: number | null) => (n === null ? "—" : n.toLocaleString());
  const hasConstraints = c.primary_key || c.unique || c.foreign_keys.length > 0 || c.checks.length > 0 || !c.nullable;
  return (
    <div className="focus">
      <div className="focus-head">
        <div>
          <div className="focus-title">{c.name}</div>
          <div className="muted small">{t.owner}.{t.table} · {c.data_type}{c.nullable ? "" : " · NOT NULL"}</div>
        </div>
        {onClose && <button className="ghost icon" onClick={onClose} aria-label="Close"><span>✕</span></button>}
      </div>
      <div className="badges">{columnBadges(c).map((b, i) => <span key={i} className={`badge ${b.tone}`} title={b.title}>{b.text}</span>)}</div>

      <Section title="Indexes" count={c.indexes.length}><IndexRows t={t} column={c} onOpen={onOpen} /></Section>
      <Section title="Triggers" count={c.triggers.length}><TriggerRows t={t} column={c} onOpen={onOpen} /></Section>
      <Section title="Sequences" count={c.sequences.length}><SequenceRows t={t} column={c} onOpen={onOpen} /></Section>
      {hasConstraints && <Section title="Constraints"><ConstraintRows t={t} column={c} onOpen={onOpen} /></Section>}
      <Section title="Statistics">
        <div className="kv"><span>Distinct values</span><b>{fmt(c.distinct)}</b></div>
        <div className="kv"><span>Nulls</span><b>{fmt(c.nulls)}</b></div>
        {c.histogram && <div className="kv"><span>Histogram</span><b>{c.histogram.toLowerCase()}</b></div>}
        {c.distinct === null && <div className="muted small">No optimizer statistics gathered for this column.</div>}
      </Section>
      <Section title="Comment">
        <CommentEditor api={api} conn={conn} env={env} owner={t.owner} table={t.table} column={c.name} current={c.comment} onSaved={onSaved} onError={onError} />
      </Section>
    </div>
  );
}
