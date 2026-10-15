import type { AskContext, AskInput, ContextPolicy, Plan, SqlIssue, ClientStatus, ConnTest, TnsList, DataDiffOptions, DataSide, SchemaDiff, SidePreview, TextDiffResult, TextOptions, AiConfig, AiEvent, AiStatus, AiTest, ChatRequest, Completions, DossierOptions, KbExported, KbPreview, KbSummary, GNode, GraphFocus, GraphInfo, PackageAnalysis, Subgraph, ConnInfo, ExportRequest, ExportSource, FileProfile, ImportOptions, JobEvent, MappingPlan, PlanInput, Purpose, ReadOptions, RecommendationInfo, HistoryHit, HistoryQuery, HistoryStatus, Hit, ObjectDeps, ObjectEntry, Page, Profile, RefreshStats, RunResult, SequenceInfo, StatementRange, Step, TableInsight, TableRef, WorksheetFilter, WorksheetRec, WorksheetSave } from "./types";
import { mockApi } from "./mock";

/** Every call after `connect` is scoped to one connection id, so several databases can be open at once. */
export interface Api {
  listProfiles(): Promise<Profile[]>;
  testConnection(profile: Profile, password: string): Promise<ConnTest>;
  tnsAliases(path: string): Promise<TnsList>;
  clientStatus(): Promise<ClientStatus>;
  saveProfile(p: Profile): Promise<void>;
  deleteProfile(name: string): Promise<void>;
  connect(profile: Profile, password: string): Promise<ConnInfo>;
  disconnect(conn: string): Promise<void>;
  /** `owner` and `worksheet` only label the history record; the statement runs as the connection's user. */
  runQuery(conn: string, sql: string, confirmed: boolean, pageSize: number, owner: string, worksheet: string | null): Promise<RunResult>;
  fetchMore(conn: string, handle: number, pageSize: number): Promise<Page>;
  closeQuery(conn: string, handle: number): Promise<void>;
  cancelQuery(conn: string): Promise<void>;
  commit(conn: string): Promise<void>;
  rollback(conn: string): Promise<void>;
  listSchemas(conn: string): Promise<string[]>;
  objectCounts(conn: string, owner: string): Promise<Record<string, number>>;
  listObjects(conn: string, owner: string, kind: string): Promise<ObjectEntry[]>;
  objectSource(conn: string, owner: string, name: string, kind: string): Promise<string>;
  objectDdl(conn: string, owner: string, name: string, kind: string): Promise<string>;
  sequenceInfo(conn: string, owner: string, name: string): Promise<SequenceInfo | null>;
  objectDependencies(conn: string, owner: string, name: string): Promise<ObjectDeps>;
  previewTable(conn: string, owner: string, table: string, limit: number): Promise<RunResult>;
  refreshMetadata(conn: string, owner: string, includeSource: boolean): Promise<RefreshStats>;
  tableInsight(conn: string, owner: string, table: string): Promise<TableInsight>;
  complete(conn: string, sql: string, cursor: number, owner: string): Promise<Completions>;
  references(sql: string): Promise<TableRef[]>;
  /** Statements of a script with UTF-16 offsets, for run-current / run-all. */
  statements(sql: string): Promise<StatementRange[]>;
  searchObjects(conn: string, text: string, owner: string | null, kinds: string[], limit: number): Promise<Hit[]>;
  previewComment(owner: string, table: string, column: string | null, text: string): Promise<string>;
  saveComment(conn: string, owner: string, table: string, column: string | null, text: string): Promise<string>;
  // history + worksheets (local SQLite, survives restarts)
  historyStatus(): Promise<HistoryStatus>;
  historySetEnabled(enabled: boolean): Promise<void>;
  historySearch(query: HistoryQuery): Promise<HistoryHit[]>;
  historySuggest(typed: string, connKey: string | null, limit: number): Promise<HistoryHit[]>;
  historyPin(hash: string, pinned: boolean): Promise<void>;
  historyForget(hash: string): Promise<void>;
  historyPurge(beforeMs: number): Promise<number>;
  wsSave(ws: WorksheetSave): Promise<WorksheetRec | null>;
  wsClose(uuid: string): Promise<void>;
  wsPin(uuid: string, pinned: boolean): Promise<void>;
  wsDelete(uuid: string): Promise<void>;
  wsList(filter: WorksheetFilter): Promise<WorksheetRec[]>;
  wsGet(uuid: string): Promise<WorksheetRec | null>;
  wsSteps(uuid: string): Promise<Step[]>;
  // export / import (long work runs as a job and reports through onJob)
  pickSavePath(defaultName: string): Promise<string | null>;
  pickOpenPath(extensions: string[]): Promise<string | null>;
  onJob(cb: (e: JobEvent) => void): Promise<() => void>;
  jobCancel(id: string): Promise<void>;
  exportRecommend(conn: string, source: ExportSource, purpose: Purpose): Promise<RecommendationInfo>;
  exportStart(conn: string, request: ExportRequest): Promise<string>;
  importProfile(path: string, read: ReadOptions): Promise<{ id: string; profile: FileProfile }>;
  importPlan(conn: string, owner: string, table: string, profileId: string, input: PlanInput): Promise<MappingPlan>;
  importDdl(profileId: string, owner: string, table: string): Promise<string>;
  importCreateTable(conn: string, ddl: string): Promise<void>;
  importValidateStart(path: string, read: ReadOptions, plan: MappingPlan, trim: boolean): Promise<string>;
  importStart(conn: string, path: string, read: ReadOptions, plan: MappingPlan, options: ImportOptions): Promise<string>;
  // packages + linkage graph (computed once per schema, then cached until refresh)
  packageAnalysis(conn: string, owner: string, refresh: boolean): Promise<PackageAnalysis>;
  graphInfo(conn: string, owner: string, refresh: boolean): Promise<GraphInfo>;
  graphSearch(conn: string, owner: string, query: string): Promise<GNode[]>;
  graphFocus(conn: string, owner: string, focus: GraphFocus): Promise<Subgraph>;
  // knowledge base (every object and link with its evidence) and markdown dossiers
  kbBuild(conn: string, owner: string): Promise<KbSummary>;
  kbStatus(conn: string, owner: string): Promise<KbSummary | null>;
  kbPreview(conn: string, owner: string, options: DossierOptions): Promise<KbPreview>;
  kbExport(conn: string, owner: string, options: DossierOptions, path: string, zip: boolean): Promise<KbExported>;
  // AI provider layer (the token lives in the app's memory only; calls are made by the core, not by scripts)
  aiStatus(): Promise<AiStatus>;
  aiGetConfig(): Promise<AiConfig>;
  aiSaveConfig(config: AiConfig): Promise<AiStatus>;
  aiSetToken(token: string, expiresAtMs: number | null): Promise<AiStatus>;
  aiClearToken(): Promise<AiStatus>;
  aiTest(): Promise<AiTest>;
  aiChatStart(request: ChatRequest): Promise<string>;
  aiCancel(id: string): Promise<void>;
  explainPlan(conn: string, sql: string): Promise<Plan>;
  askContext(conn: string, owner: string, input: AskInput, policy: ContextPolicy): Promise<AskContext>;
  askStart(conn: string, owner: string, input: AskInput, policy: ContextPolicy): Promise<{ id: string; context: AskContext }>;
  askValidate(conn: string, owner: string, sql: string): Promise<SqlIssue[]>;
  askExtractSql(answer: string): Promise<string[]>;
  onAi(cb: (e: AiEvent) => void): Promise<() => void>;
  // compare (text, data by key, schema); data compare runs as a job and reports through onJob
  textDiff(a: string, b: string, options: TextOptions, context: number | null): Promise<TextDiffResult>;
  textPatch(a: string, b: string, options: TextOptions, aName: string, bName: string, context: number): Promise<string>;
  textReadFile(path: string): Promise<string>;
  textWriteFile(path: string, content: string): Promise<void>;
  dataSidePreview(side: DataSide): Promise<SidePreview>;
  dataDiffStart(a: DataSide, b: DataSide, options: DataDiffOptions, outPath: string | null): Promise<string>;
  schemaCompare(connA: string, ownerA: string, connB: string, ownerB: string, includeCode: boolean): Promise<SchemaDiff>;
}

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriApi(): Promise<Api> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    listProfiles: () => invoke("list_profiles"),
    testConnection: (profile, password) => invoke("test_connection", { profile, password }),
    tnsAliases: (path) => invoke("tns_aliases", { path }),
    clientStatus: () => invoke("client_status"),
    saveProfile: (profile) => invoke("save_profile", { profile }),
    deleteProfile: (name) => invoke("delete_profile", { name }),
    connect: (profile, password) => invoke("connect", { profile, password }),
    disconnect: (conn) => invoke("disconnect", { conn }),
    runQuery: (conn, sql, confirmed, pageSize, owner, worksheet) => invoke("run_query", { conn, sql, confirmed, pageSize, owner, worksheet }),
    fetchMore: (conn, handle, pageSize) => invoke("fetch_more", { conn, handle, pageSize }),
    closeQuery: (conn, handle) => invoke("close_query", { conn, handle }),
    cancelQuery: (conn) => invoke("cancel_query", { conn }),
    commit: (conn) => invoke("commit", { conn }),
    rollback: (conn) => invoke("rollback", { conn }),
    listSchemas: (conn) => invoke("list_schemas", { conn }),
    objectCounts: (conn, owner) => invoke("object_counts", { conn, owner }),
    listObjects: (conn, owner, kind) => invoke("list_objects", { conn, owner, kind }),
    objectSource: (conn, owner, name, kind) => invoke("object_source", { conn, owner, name, kind }),
    objectDdl: (conn, owner, name, kind) => invoke("object_ddl", { conn, owner, name, kind }),
    sequenceInfo: (conn, owner, name) => invoke("sequence_info", { conn, owner, name }),
    objectDependencies: (conn, owner, name) => invoke("object_dependencies", { conn, owner, name }),
    previewTable: (conn, owner, table, limit) => invoke("preview_table", { conn, owner, table, limit }),
    refreshMetadata: (conn, owner, includeSource) => invoke("refresh_metadata", { conn, owner, includeSource }),
    tableInsight: (conn, owner, table) => invoke("table_insight", { conn, owner, table }),
    complete: (conn, sql, cursor, owner) => invoke("complete", { conn, sql, cursor, owner }),
    references: (sql) => invoke("references", { sql }),
    statements: (sql) => invoke("statements", { sql }),
    searchObjects: (conn, text, owner, kinds, limit) => invoke("search_objects", { conn, text, owner, kinds, limit }),
    previewComment: (owner, table, column, text) => invoke("preview_comment", { owner, table, column, text }),
    saveComment: (conn, owner, table, column, text) => invoke("save_comment", { conn, owner, table, column, text }),
    historyStatus: () => invoke("history_status"),
    historySetEnabled: (enabled) => invoke("history_set_enabled", { enabled }),
    historySearch: (query) => invoke("history_search", { query }),
    historySuggest: (typed, connKey, limit) => invoke("history_suggest", { typed, connKey, limit }),
    historyPin: (hash, pinned) => invoke("history_pin", { hash, pinned }),
    historyForget: (hash) => invoke("history_forget", { hash }),
    historyPurge: (beforeMs) => invoke("history_purge", { beforeMs }),
    wsSave: (ws) => invoke("ws_save", { ws }),
    wsClose: (uuid) => invoke("ws_close", { uuid }),
    wsPin: (uuid, pinned) => invoke("ws_pin", { uuid, pinned }),
    wsDelete: (uuid) => invoke("ws_delete", { uuid }),
    wsList: (filter) => invoke("ws_list", { filter }),
    wsGet: (uuid) => invoke("ws_get", { uuid }),
    wsSteps: (uuid) => invoke("ws_steps", { uuid }),
    async pickSavePath(defaultName) {
      const { save } = await import("@tauri-apps/plugin-dialog");
      return save({ defaultPath: defaultName });
    },
    async pickOpenPath(extensions) {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const r = await open({ multiple: false, directory: false, ...(extensions.length ? { filters: [{ name: "Files", extensions }] } : {}) });
      return typeof r === "string" ? r : null;
    },
    async onJob(cb) {
      const { listen } = await import("@tauri-apps/api/event");
      const a = await listen("job-progress", (e) => cb({ type: "progress", data: e.payload as never }));
      const b = await listen("job-done", (e) => cb({ type: "done", data: e.payload as never }));
      return () => { a(); b(); };
    },
    jobCancel: (id) => invoke("job_cancel", { id }),
    aiStatus: () => invoke("ai_status"),
    aiGetConfig: () => invoke("ai_get_config"),
    aiSaveConfig: (config) => invoke("ai_save_config", { config }),
    aiSetToken: (token, expiresAtMs) => invoke("ai_set_token", { token, expiresAtMs }),
    aiClearToken: () => invoke("ai_clear_token"),
    aiTest: () => invoke("ai_test"),
    aiChatStart: (request) => invoke("ai_chat_start", { request }),
    aiCancel: (id) => invoke("ai_cancel", { id }),
    explainPlan: (conn, sql) => invoke("explain_plan", { conn, sql }),
    askContext: (conn, owner, input, policy) => invoke("ask_context", { conn, owner, input, policy }),
    askStart: (conn, owner, input, policy) => invoke("ask_start", { conn, owner, input, policy }),
    askValidate: (conn, owner, sql) => invoke("ask_validate", { conn, owner, sql }),
    askExtractSql: (answer) => invoke("ask_extract_sql", { answer }),
    textDiff: (a, b, options, context) => invoke("text_diff", { a, b, options, context }),
    textPatch: (a, b, options, aName, bName, context) => invoke("text_patch", { a, b, options, aName, bName, context }),
    textReadFile: (path) => invoke("text_read_file", { path }),
    textWriteFile: (path, content) => invoke("text_write_file", { path, content }),
    dataSidePreview: (side) => invoke("data_side_preview", { side }),
    dataDiffStart: (a, b, options, outPath) => invoke("data_diff_start", { a, b, options, outPath }),
    schemaCompare: (connA, ownerA, connB, ownerB, includeCode) => invoke("schema_compare", { connA, ownerA, connB, ownerB, includeCode }),
    async onAi(cb) {
      const { listen } = await import("@tauri-apps/api/event");
      return listen("ai-event", (e) => cb(e.payload as never));
    },
    exportRecommend: (conn, source, purpose) => invoke("export_recommend", { conn, source, purpose }),
    exportStart: (conn, request) => invoke("export_start", { conn, request }),
    importProfile: (path, read) => invoke("import_profile", { path, read }),
    importPlan: (conn, owner, table, profileId, input) => invoke("import_plan", { conn, owner, table, profileId, input }),
    importDdl: (profileId, owner, table) => invoke("import_ddl", { profileId, owner, table }),
    importCreateTable: (conn, ddl) => invoke("import_create_table", { conn, ddl }),
    importValidateStart: (path, read, plan, trim) => invoke("import_validate_start", { path, read, plan, trim }),
    importStart: (conn, path, read, plan, options) => invoke("import_start", { conn, path, read, plan, options }),
    packageAnalysis: (conn, owner, refresh) => invoke("package_analysis", { conn, owner, refresh }),
    graphInfo: (conn, owner, refresh) => invoke("graph_info", { conn, owner, refresh }),
    graphSearch: (conn, owner, query) => invoke("graph_search", { conn, owner, query }),
    graphFocus: (conn, owner, focus) => invoke("graph_focus", { conn, owner, focus }),
    kbBuild: (conn, owner) => invoke("kb_build", { conn, owner }),
    kbStatus: (conn, owner) => invoke("kb_status", { conn, owner }),
    kbPreview: (conn, owner, options) => invoke("kb_preview", { conn, owner, options }),
    kbExport: (conn, owner, options, path, zip) => invoke("kb_export", { conn, owner, options, path, zip }),
  };
}

/** Real backend inside Tauri; an in-browser demo (no database) otherwise. */
export const apiPromise: Promise<Api> = inTauri ? tauriApi() : Promise.resolve(mockApi);
export const isDemo = !inTauri;
