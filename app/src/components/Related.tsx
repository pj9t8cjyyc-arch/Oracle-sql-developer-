import type { ReactNode } from "react";
import { Icon, KindIcon } from "../ui-icons";
import type { ColumnInsight, ObjectKind, SeqUse, TableInsight } from "../types";

export type OpenFn = (kind: ObjectKind, owner: string, name: string, parent?: string) => void;

export const REASON: Record<string, string> = {
  update_of: "fires when this column is updated (UPDATE OF)",
  referenced: "reads or writes this column (:NEW / :OLD)",
  any_update: "fires on any UPDATE of the row",
};

function Tag({ tone, children, title }: { tone?: "warn" | "ok" | "muted"; children: ReactNode; title?: string }) {
  return <span className={`tag ${tone ?? ""}`} title={title}>{children}</span>;
}

function Item({ kind, name, sub, tags, onClick, children }: { kind: ObjectKind; name: string; sub?: ReactNode; tags?: ReactNode; onClick?: () => void; children?: ReactNode }) {
  return (
    <div className={`rel-item ${onClick ? "clickable" : ""}`} onClick={onClick}>
      <KindIcon kind={kind} size={15} />
      <div className="rel-main">
        <div className="rel-title"><span className="rel-name">{name}</span>{tags}</div>
        {sub && <div className="rel-sub">{sub}</div>}
        {children}
      </div>
      {onClick && <Icon name="out" size={13} className="rel-go" />}
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="rel-empty">{children}</div>;
}

// ------------------------------------------------------------------ indexes
export function IndexRows({ t, column, onOpen }: { t: TableInsight; column?: ColumnInsight; onOpen: OpenFn }) {
  if (column) {
    if (column.indexes.length === 0) {
      return <Empty>{column.primary_key ? "Covered by the primary key." : <>No index on this column. Filtering or joining on it scans the table{column.foreign_keys.length ? <> — and deleting from the parent table can lock this one (unindexed foreign key)</> : null}.</>}</Empty>;
    }
    return (
      <>
        {column.indexes.map((i) => {
          const full = t.indexes.find((x) => x.name === i.name);
          return (
            <Item key={i.name} kind="INDEX" name={i.name} onClick={() => onOpen("INDEX", t.owner, i.name, t.table)}
              tags={<>{i.unique && <Tag tone="ok">unique</Tag>}{!i.valid && <Tag tone="warn">unusable</Tag>}</>}
              sub={<>position {i.position}{i.leading ? " · leading column" : " · not the leading column — a filter on this column alone cannot use this index well"}{full && full.columns.length > 1 ? ` · (${full.columns.join(", ")})` : ""}</>} />
          );
        })}
      </>
    );
  }
  if (t.indexes.length === 0) return <Empty>No indexes on this table.</Empty>;
  return (
    <>
      {t.indexes.map((i) => (
        <Item key={i.name} kind="INDEX" name={i.name} onClick={() => onOpen("INDEX", t.owner, i.name, t.table)}
          tags={<>{i.unique && <Tag tone="ok">unique</Tag>}{i.index_type !== "NORMAL" && <Tag tone="muted">{i.index_type.toLowerCase()}</Tag>}{!i.valid && <Tag tone="warn">unusable</Tag>}</>}
          sub={<>{i.columns.join(", ")}</>} />
      ))}
    </>
  );
}

// ----------------------------------------------------------------- triggers
export function TriggerRows({ t, column, onOpen }: { t: TableInsight; column?: ColumnInsight; onOpen: OpenFn }) {
  const rows = column ? column.triggers.map((ct) => ({ ct, info: t.triggers.find((x) => x.name === ct.name) })) : t.triggers.map((info) => ({ ct: undefined, info }));
  if (rows.length === 0) return <Empty>{column ? "No trigger touches this column." : "No triggers on this table."}</Empty>;
  return (
    <>
      {rows.map(({ ct, info }) => {
        const name = ct?.name ?? info!.name;
        const enabled = ct?.enabled ?? info!.enabled;
        return (
          <Item key={name} kind="TRIGGER" name={name} onClick={() => onOpen("TRIGGER", info?.owner ?? t.owner, name, t.table)}
            tags={!enabled ? <Tag tone="warn">disabled</Tag> : undefined}
            sub={<>{ct?.timing ?? info!.timing} {ct?.event ?? info!.event}{ct ? <> · {REASON[ct.reason]}</> : info!.columns.length ? <> · columns: {info!.columns.join(", ")}</> : null}</>}>
            {info?.when_clause && <div className="rel-sub mono">WHEN {info.when_clause}</div>}
            {info && info.sequences.length > 0 && <div className="rel-sub">uses sequence {info.sequences.join(", ")}</div>}
          </Item>
        );
      })}
    </>
  );
}

// ---------------------------------------------------------------- sequences
const VIA: Record<string, string> = { identity: "identity column", trigger: "assigned in trigger", default: "referenced by the table (e.g. a column DEFAULT)" };

function seqProps(s: SeqUse): string {
  const p: string[] = [];
  if (s.generation) p.push(`GENERATED ${s.generation}`);
  if (s.increment) p.push(`step ${s.increment}`);
  if (s.cache) p.push(`cache ${s.cache}`);
  if (s.last_number) p.push(`next ${s.last_number}`);
  if (s.cycle) p.push("cycles");
  return p.join(" · ");
}

export function SequenceRows({ t, column, onOpen }: { t: TableInsight; column?: ColumnInsight; onOpen: OpenFn }) {
  const uses: SeqUse[] = column
    ? column.sequences.map((cs) => t.sequences.find((u) => u.name === cs.name && u.via === cs.via && u.via_name === cs.via_name) ?? ({ owner: cs.owner, name: cs.name, via: cs.via, via_name: cs.via_name, columns: [column.name] } as SeqUse))
    : t.sequences;
  if (uses.length === 0) {
    return <Empty>{column ? "No sequence is known to fill this column." : "No sequences found for this table."}</Empty>;
  }
  return (
    <>
      {uses.map((s) => (
        <Item key={`${s.name}${s.via}${s.via_name}`} kind="SEQUENCE" name={`${s.owner}.${s.name}`} onClick={s.via === "identity" && s.name.startsWith("ISEQ$$") ? undefined : () => onOpen("SEQUENCE", s.owner, s.name)}
          tags={<Tag tone="muted">{s.via}</Tag>}
          sub={<>{VIA[s.via]}{s.via === "trigger" ? ` ${s.via_name}` : ""}{!column && s.columns.length > 0 ? ` · fills ${s.columns.join(", ")}` : ""}{!column && s.via === "default" ? " · column not determined" : ""}</>}>
          {seqProps(s) && <div className="rel-sub">{seqProps(s)}</div>}
        </Item>
      ))}
    </>
  );
}

// -------------------------------------------------------------- constraints
export function ConstraintRows({ t, column, onOpen }: { t: TableInsight; column?: ColumnInsight; onOpen: OpenFn }) {
  const cols = column ? [column] : t.columns;
  const pk = t.columns.filter((c) => c.primary_key);
  const rows: ReactNode[] = [];
  if (pk.length && (!column || column.primary_key)) rows.push(<Item key="pk" kind="INDEX" name="Primary key" sub={pk.map((c) => c.name).join(", ")} tags={<Tag tone="ok">PK</Tag>} />);
  // One NOT NULL row for the table view instead of one per column.
  const notNull = cols.filter((c) => !c.nullable && !c.primary_key).map((c) => c.name);
  if (notNull.length) rows.push(<Item key="nn" kind="TABLE" name="NOT NULL" sub={notNull.join(", ")} />);
  for (const c of cols) {
    if (c.unique) rows.push(<Item key={`u${c.name}`} kind="INDEX" name="Unique" sub={c.name} tags={<Tag tone="ok">UQ</Tag>} />);
    for (const f of c.foreign_keys) {
      rows.push(
        <Item key={`f${c.name}${f.constraint}`} kind="TABLE" name={f.constraint} onClick={() => onOpen("TABLE", f.ref_owner, f.ref_table)}
          tags={<Tag tone="muted">FK</Tag>} sub={<>{c.name} → {f.ref_owner}.{f.ref_table}.{f.ref_column}</>} />,
      );
    }
    for (const ck of c.checks) rows.push(<Item key={`c${c.name}${ck}`} kind="TABLE" name="Check" sub={<span className="mono">{ck}</span>} tags={<Tag tone="muted">CK</Tag>} />);
  }
  return rows.length ? <>{rows}</> : <Empty>No constraints{column ? " on this column" : ""}.</Empty>;
}

/** Tables whose foreign keys point here. */
export function ReferencedByRows({ t, onOpen }: { t: TableInsight; onOpen: OpenFn }) {
  if (t.referenced_by.length === 0) return <Empty>No table references this one.</Empty>;
  return <>{t.referenced_by.map((r) => <Item key={r.constraint} kind="TABLE" name={`${r.owner}.${r.table}`} sub={r.constraint} onClick={() => onOpen("TABLE", r.owner, r.table)} />)}</>;
}
