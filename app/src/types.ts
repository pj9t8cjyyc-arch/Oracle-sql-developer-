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
