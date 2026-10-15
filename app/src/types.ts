export type Cell = string | null;
export type Row = Cell[];
export interface Column { name: string; data_type: string; nullable: boolean }
export interface Warning { severity: "info" | "warning" | "danger"; message: string }
export interface Analysis {
  kind: string;
  sql: string;
  warnings: Warning[];
  requires_confirmation: boolean;
  blocked: string | null;
  read_only: boolean;
}
export type RunResult =
  | { kind: "needs_confirmation"; analysis: Analysis }
  | { kind: "rows"; handle: number; columns: Column[]; rows: Row[]; done: boolean; analysis: Analysis }
  | { kind: "affected"; count: number; analysis: Analysis }
  | { kind: "done"; analysis: Analysis };
export interface Page { rows: Row[]; done: boolean; truncated: boolean }
export type Environment = "dev" | "test" | "prod";
export interface Profile {
  name: string;
  host: string;
  port: number;
  service: string;
  user: string;
  environment: Environment;
}

// ---- Phase 1: insight, completion, search ----
export interface FkRef { constraint: string; ref_owner: string; ref_table: string; ref_column: string }
export interface ColumnSeq { owner: string; name: string; via: "identity" | "trigger"; via_name: string }
export interface SeqUse {
  owner: string; name: string; via: "identity" | "trigger" | "default"; via_name: string;
  generation: string | null; min: string | null; max: string | null; increment: string | null;
  cycle: boolean; cache: string | null; last_number: string | null; columns: string[];
}
export interface ColumnIndex { name: string; position: number; unique: boolean; leading: boolean; valid: boolean }
export interface ColumnTrigger { name: string; timing: string; event: string; enabled: boolean; reason: "update_of" | "referenced" | "any_update" }
export interface ColumnInsight {
  name: string; data_type: string; nullable: boolean; comment: string | null;
  distinct: number | null; nulls: number | null; histogram: string | null;
  primary_key: boolean; unique: boolean; foreign_keys: FkRef[]; checks: string[];
  indexes: ColumnIndex[]; triggers: ColumnTrigger[]; sequences: ColumnSeq[];
}
export interface IndexInfo { name: string; unique: boolean; valid: boolean; index_type: string; columns: string[] }
export interface TriggerInfo { name: string; owner: string; timing: string; event: string; enabled: boolean; columns: string[]; when_clause: string | null; sequences: string[] }
export interface Dependency { owner: string; name: string; object_type: string }
export interface TableInsight {
  owner: string; table: string; comment: string | null;
  columns: ColumnInsight[]; indexes: IndexInfo[]; triggers: TriggerInfo[]; sequences: SeqUse[];
  referenced_by: { owner: string; table: string; constraint: string }[];
  uses: Dependency[]; used_by: Dependency[];
}
export interface TableRef { schema: string | null; name: string; alias: string | null }
export interface Suggestion { label: string; insert: string; kind: "table" | "view" | "column" | "schema" | "alias" | "keyword" | "join"; detail: string | null; score: number }
export interface Completions { from: number; items: Suggestion[] }
export interface Hit { kind: string; owner: string; name: string; parent: string; line: number | null; snippet: string; score: number }
export interface RefreshStats { tables: number; columns: number; indexed_docs: number }

// ---- Workspace: dictionary browser ----
export type ObjectKind = "TABLE" | "VIEW" | "MATERIALIZED VIEW" | "INDEX" | "SEQUENCE" | "TRIGGER" | "PROCEDURE" | "FUNCTION" | "PACKAGE" | "TYPE" | "SYNONYM";
export const KINDS: [ObjectKind, string][] = [
  ["TABLE", "Tables"], ["VIEW", "Views"], ["MATERIALIZED VIEW", "Materialized views"], ["INDEX", "Indexes"], ["SEQUENCE", "Sequences"],
  ["TRIGGER", "Triggers"], ["PROCEDURE", "Procedures"], ["FUNCTION", "Functions"], ["PACKAGE", "Packages"], ["TYPE", "Types"], ["SYNONYM", "Synonyms"],
];
export interface ObjectEntry { name: string; status: string; ok: boolean; detail: string | null; parent: string | null }
export interface SequenceInfo {
  owner: string; name: string; min: string; max: string; increment: string;
  cycle: boolean; ordered: boolean; cache: string; last_number: string;
}
export interface ObjectDeps { uses: Dependency[]; used_by: Dependency[] }

// ---- History + worksheets (local SQLite) ----
export interface ConnInfo { id: string; key: string }
export interface HistoryHit {
  hash: string; sql: string; kind: string; tables: string[];
  runs: number; ok_runs: number; first_run: number; last_run: number; avg_ms: number | null;
  last_status: "ok" | "error" | "cancelled"; last_conn_name: string; last_conn_key: string; last_owner: string;
  pinned: boolean; snippet: string | null; score: number;
}
export interface HistoryQuery {
  text?: string; conn_key?: string | null; only_ok?: boolean; since?: number | null; until?: number | null;
  kinds?: string[]; pinned_only?: boolean; limit?: number; offset?: number;
}
export interface OutlineItem { kind: string; tables: string; preview: string; start: number; end: number }
export interface WorksheetRec {
  uuid: string; conn_key: string; conn_name: string; owner: string; title: string; custom_title: boolean;
  sql: string; outline: OutlineItem[]; created_at: number; updated_at: number; closed_at: number | null;
  pinned: boolean; position: number; runs: number;
}
export interface WorksheetSave { uuid: string; conn_key: string; conn_name: string; owner: string; title: string | null; sql: string; position: number }
export interface WorksheetFilter { state?: "open" | "closed" | "all"; text?: string; conn_key?: string | null; limit?: number }
export interface Step { at: number; hash: string; sql: string; kind: string; status: string; duration_ms: number; rows: number | null; error: string | null }
export interface HistoryStatus { enabled: boolean; persistent: boolean; stats: { executions: number; distinct_queries: number; worksheets: number; first_at: number | null; size_bytes: number } }
export interface StatementRange { start: number; end: number; kind: string; text: string }
