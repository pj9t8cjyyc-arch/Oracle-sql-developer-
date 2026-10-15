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

// ---- Export / import ----
export type ExportFormat = "csv" | "tsv" | "json_lines" | "xlsx" | "sql_insert";
export type Purpose = "spreadsheet" | "load_oracle" | "exchange" | "archive";
export interface Alternative { format: ExportFormat; verdict: "good" | "caution" | "avoid"; note: string }
export interface Recommendation { format: ExportFormat; gzip: boolean; split_rows: number | null; reasons: string[]; alternatives: Alternative[]; tip: string | null }
export interface RecommendationInfo { est_rows: number | null; from_statistics: boolean; recommendation: Recommendation }
export type ExportSource = { type: "query"; sql: string } | { type: "table"; owner: string; table: string; where_clause: string | null };
export interface ExportOptions {
  format: ExportFormat; header: boolean; delimiter: string; null_text: string; bom: boolean; gzip: boolean;
  sanitize_formulas: boolean; split_rows: number | null; insert_table: string | null; rows_per_commit: number;
  sqlplus_directives: boolean; xlsx_sheet_rows: number; batch_size: number;
}
export interface ExportRequest { source: ExportSource; path: string; options: ExportOptions; row_limit: number | null; overwrite: boolean }
export interface ExportReport { rows: number; bytes: number; files: string[]; elapsed_ms: number; rows_per_sec: number; warnings: string[]; cancelled: boolean }
export interface JobProgress { id: string; kind: "export" | "import" | "validate"; rows: number; bytes: number; rejected: number; elapsed_ms: number }
export interface JobDone { id: string; kind: "export" | "import" | "validate"; ok: boolean; cancelled: boolean; error: string | null; report: unknown }
export type JobEvent = { type: "progress"; data: JobProgress } | { type: "done"; data: JobDone };

export type ColKindName = "empty" | "integer" | "decimal" | "date" | "timestamp" | "boolean" | "text";
export interface ColProfile {
  index: number; name: string; kind: ColKindName; nulls: number; non_null: number; max_chars: number; max_bytes: number;
  int_digits: number; scale: number; date_formats: string[]; date_ambiguous: boolean;
  number_style: { decimal: string; thousands: string | null } | null; number_ambiguous: boolean; leading_zeros: boolean; samples: string[];
}
export interface FileProfile {
  columns: ColProfile[]; rows_scanned: number; more_rows: boolean; ragged_rows: number; format: string; delimiter: string | null;
  has_header: boolean; encoding: string; sheets: string[]; warnings: string[];
}
export interface ReadOptions { delimiter?: string | null; has_header?: boolean | null; encoding?: string | null; sheet?: string | null }
export interface TargetCol {
  name: string; data_type: string; display: string; char_length: number; data_length: number; char_used: string;
  precision: number | null; scale: number | null; nullable: boolean; insertable: boolean; note: string | null;
}
export type Conversion = { type: "text" } | { type: "number"; decimal: string } | { type: "date"; family: string } | { type: "timestamp"; family: string } | { type: "boolean" };
export interface PlanIssue { severity: "info" | "warning" | "error"; message: string; fix: string | null }
export interface PlanChoice { label: string; value: string }
export interface ColumnMap { target: TargetCol; file_index: number | null; file_name: string | null; conversion: Conversion; issues: PlanIssue[]; choices: PlanChoice[]; suggested_type: string | null }
export interface MappingPlan { owner: string; table: string; maps: ColumnMap[]; unmapped_file: string[]; by_position: boolean; errors: number; warnings: number }
export interface PlanInput { assignment?: Record<string, number | null> | null; date_family?: Record<string, string>; number_decimal?: Record<string, string> }
export interface Reject { row: number; reason: string; fields: string[] }
export interface ValidationReport { rows: number; ok: number; rejected: number; by_column: [string, number][]; samples: Reject[]; cancelled: boolean }
export interface ImportOptions { batch_size: number; commit: "at_end" | "per_batch"; on_error: "stop" | "skip"; max_rejects: number; truncate_first: boolean; trim: boolean; append_hint: boolean }
export interface ImportReport {
  read: number; inserted: number; rejected: number; committed: boolean; rolled_back: boolean; reject_file: string | null;
  stopped: string | null; cancelled: boolean; elapsed_ms: number; rows_per_sec: number; samples: Reject[];
}

// ---- package explorer + linkage graph
export interface MemberRef { package: string; member: string }
export interface PkgMember {
  name: string; kind: "PROCEDURE" | "FUNCTION"; signature: string; overload: string | null; public: boolean;
  calls: MemberRef[]; reads: string[]; writes: string[]; called_by: MemberRef[]; internal_refs: number;
  source: "plscope" | "scan"; scan_only: string[];
  status: "used" | "unreferenced" | "unused_private";
}
export interface TableUse { table: string; read: boolean; write: boolean }
export interface PackageDetail {
  name: string; spec_status: string; body_status: string | null; wrapped: boolean; body_lines: number; members: PkgMember[];
  calls_packages: string[]; called_by: string[]; tables: TableUse[]; unreferenced: number; unused_private: number;
  analysis: "plscope" | "scan" | "none"; unattributed: number;
}
export interface PackageAnalysis { owner: string; packages: PackageDetail[]; wrapped: number; invalid: number; plscope_packages: number; scope_error: string | null }

export interface GNode { id: string; owner: string; name: string; kind: string }
export interface GEdge { from: string; to: string; kind: "uses" | "fk" | "indexes" | "reads" | "writes" | "calls" | "runs" }
export interface GraphInfo { owner: string; nodes: number; edges: number; kinds: Record<string, number> }
export type Direction = "uses" | "used_by" | "both";
export interface GraphFocus { id: string; direction: Direction; hops: number; kinds: string[] | null; max_nodes?: number | null }
export interface SNode extends GNode { depth: number; more: number }
export interface Subgraph { focus: string; nodes: SNode[]; edges: GEdge[]; truncated: boolean }

// ---- knowledge base + markdown export
export interface Gap { severity: "warning" | "info"; area: string; message: string }
export interface Coverage { gaps: Gap[]; packages: number; packages_scanned: number; packages_wrapped: number; packages_plscope: number; members: number; dynamic_sql_sites: number }
export interface KbSummary {
  owner: string; taken_at: number; tables: number; views: number; packages: number; members: number; triggers: number; indexes: number;
  sequences: number; nodes: number; edges: number; exact_links: number; heuristic_links: number; coverage: Coverage;
}
export interface DossierOptions { focus: string; direction: Direction; hops: number; include_source: boolean; max_member_lines: number; max_nodes?: number | null }
export interface KbPreview { markdown: string; files: string[]; tokens_estimate: number; truncated: boolean; missing: string[]; preview_cut: boolean }
export interface KbExported { path: string; bytes: number; files: number; tokens_estimate: number }

// ---- AI provider layer
export interface AiConfig {
  provider: "openai_compatible" | "mock"; base_url: string; path: string; model: string; auth_header: string; auth_scheme: string;
  extra_headers: Record<string, string>; timeout_secs: number; stream: boolean; max_tokens: number | null; temperature: number | null;
}
export interface AiStatus { configured: boolean; signed_in: boolean; expires_at_ms: number | null; provider: string; model: string; base_url: string; config_error: string | null }
export interface AiTest { ok: boolean; text: string; elapsed_ms: number; error: string | null; auth_required: boolean }
export type ChatRole = "system" | "user" | "assistant";
export interface ChatMessage { role: ChatRole; content: string }
export interface ChatRequest { messages: ChatMessage[]; model?: string | null; max_tokens?: number | null; temperature?: number | null }
export interface ChatResult { text: string; usage: { prompt_tokens: number; completion_tokens: number } | null; finish_reason: string | null; cancelled: boolean; elapsed_ms: number }
export type AiEvent =
  | { type: "delta"; id: string; text: string }
  | { type: "done"; id: string; result: ChatResult }
  | { type: "error"; id: string; message: string; auth_required: boolean };
