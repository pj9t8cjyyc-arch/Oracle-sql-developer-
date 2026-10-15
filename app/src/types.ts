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
export interface DbObject { name: string; object_type: string; valid: boolean }
export interface TableColumn { name: string; data_type: string; nullable: boolean }

// ---- Phase 1: insight, completion, search ----
export interface FkRef { constraint: string; ref_owner: string; ref_table: string; ref_column: string }
export interface ColumnIndex { name: string; position: number; unique: boolean; leading: boolean; valid: boolean }
export interface ColumnTrigger { name: string; timing: string; event: string; enabled: boolean; reason: "update_of" | "referenced" | "any_update" }
export interface ColumnInsight {
  name: string; data_type: string; nullable: boolean; comment: string | null;
  distinct: number | null; nulls: number | null; histogram: string | null;
  primary_key: boolean; unique: boolean; foreign_keys: FkRef[]; checks: string[];
  indexes: ColumnIndex[]; triggers: ColumnTrigger[];
}
export interface IndexInfo { name: string; unique: boolean; valid: boolean; index_type: string; columns: string[] }
export interface TriggerInfo { name: string; timing: string; event: string; enabled: boolean; columns: string[] }
export interface Dependency { owner: string; name: string; object_type: string }
export interface TableInsight {
  owner: string; table: string; comment: string | null;
  columns: ColumnInsight[]; indexes: IndexInfo[]; triggers: TriggerInfo[];
  referenced_by: { owner: string; table: string; constraint: string }[];
  uses: Dependency[]; used_by: Dependency[];
}
export interface TableRef { schema: string | null; name: string; alias: string | null }
export interface Suggestion { label: string; insert: string; kind: "table" | "view" | "column" | "schema" | "alias" | "keyword" | "join"; detail: string | null; score: number }
export interface Completions { from: number; items: Suggestion[] }
export interface Hit { kind: string; owner: string; name: string; parent: string; line: number | null; snippet: string; score: number }
export interface RefreshStats { tables: number; columns: number; indexed_docs: number }
