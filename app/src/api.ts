import type { Completions, ConnInfo, HistoryHit, HistoryQuery, HistoryStatus, Hit, ObjectDeps, ObjectEntry, Page, Profile, RefreshStats, RunResult, SequenceInfo, StatementRange, Step, TableInsight, TableRef, WorksheetFilter, WorksheetRec, WorksheetSave } from "./types";
import { mockApi } from "./mock";

/** Every call after `connect` is scoped to one connection id, so several databases can be open at once. */
export interface Api {
  listProfiles(): Promise<Profile[]>;
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
}

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriApi(): Promise<Api> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    listProfiles: () => invoke("list_profiles"),
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
  };
}

/** Real backend inside Tauri; an in-browser demo (no database) otherwise. */
export const apiPromise: Promise<Api> = inTauri ? tauriApi() : Promise.resolve(mockApi);
export const isDemo = !inTauri;
