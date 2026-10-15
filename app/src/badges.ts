import type { Api } from "./api";
import type { ColumnInsight, TableInsight, TableRef } from "./types";

export interface Badge { text: string; title: string; tone: "key" | "fk" | "idx" | "trg" | "seq" | "note" | "warn" }

export function columnBadges(c: ColumnInsight): Badge[] {
  const b: Badge[] = [];
  if (c.primary_key) b.push({ text: "PK", title: "Primary key", tone: "key" });
  else if (c.unique) b.push({ text: "UQ", title: "Unique constraint", tone: "key" });
  for (const f of c.foreign_keys) b.push({ text: "FK", title: `Foreign key → ${f.ref_owner}.${f.ref_table}.${f.ref_column}`, tone: "fk" });
  if (c.indexes.length) {
    const lead = c.indexes.filter((i) => i.leading).length;
    const bad = c.indexes.some((i) => !i.valid);
    b.push({
      text: `IX${c.indexes.length > 1 ? c.indexes.length : ""}`,
      title: c.indexes.map((i) => `${i.name} (position ${i.position}${i.leading ? ", leading" : ""}${i.valid ? "" : ", UNUSABLE"})`).join("\n") + (lead === 0 ? "\nNot the leading column of any index: filtering on it alone cannot use these indexes efficiently." : ""),
      tone: bad ? "warn" : "idx",
    });
  }
  if (c.sequences.length) b.push({ text: "SEQ", title: c.sequences.map((q) => `${q.name} (${q.via === "identity" ? "identity column" : `set by trigger ${q.via_name}`})`).join("\n"), tone: "seq" });
  if (c.triggers.length) b.push({ text: `⚡${c.triggers.length}`, title: c.triggers.map((t) => `${t.name} (${t.timing} ${t.event}${t.enabled ? "" : ", DISABLED"})`).join("\n"), tone: "trg" });
  if (c.checks.length) b.push({ text: "CK", title: c.checks.join("\n"), tone: "note" });
  if (c.comment) b.push({ text: "✎", title: c.comment, tone: "note" });
  return b;
}

/** Fetch insight once per table; cache the promise so concurrent callers share it. */
export class InsightCache {
  private m = new Map<string, Promise<TableInsight>>();
  constructor(private api: Api, private conn: string) {}
  get(owner: string, table: string): Promise<TableInsight> {
    const k = `${owner}.${table}`;
    let p = this.m.get(k);
    if (!p) {
      p = this.api.tableInsight(this.conn, owner, table);
      p.catch(() => this.m.delete(k));
      this.m.set(k, p);
    }
    return p;
  }
  invalidate(owner: string, table: string) { this.m.delete(`${owner}.${table}`); }
  clear() { this.m.clear(); }
}

/**
 * Badges per result-column name for the tables a query references. A name that
 * exists in more than one referenced table is ambiguous (we cannot tell which
 * one the result column came from), so it gets no badges rather than wrong ones.
 */
export interface QueryBadges { badges: Record<string, Badge[]>; colTable: Record<string, { owner: string; table: string }> }

export async function badgesForQuery(cache: InsightCache, refs: TableRef[], defaultOwner: string): Promise<QueryBadges> {
  const seen = new Set<string>();
  const uniq = refs.filter((r) => { const k = `${r.schema ?? defaultOwner}.${r.name}`; return seen.has(k) ? false : (seen.add(k), true); });
  const insights = (await Promise.all(uniq.map((r) => cache.get((r.schema ?? defaultOwner).toUpperCase(), r.name).catch(() => null)))).filter((x): x is TableInsight => !!x && x.columns.length > 0);
  const count = new Map<string, number>();
  for (const t of insights) for (const c of t.columns) count.set(c.name, (count.get(c.name) ?? 0) + 1);
  const badges: Record<string, Badge[]> = {};
  const colTable: QueryBadges["colTable"] = {};
  for (const t of insights) for (const c of t.columns) if (count.get(c.name) === 1) { badges[c.name] = columnBadges(c); colTable[c.name] = { owner: t.owner, table: t.table }; }
  return { badges, colTable };
}
