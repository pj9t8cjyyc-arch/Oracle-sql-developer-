import type { Completions, Hit, ObjectDeps, ObjectEntry, Page, Profile, RefreshStats, RunResult, SequenceInfo, TableInsight, TableRef } from "./types";
import { mockApi } from "./mock";

/** Every call after `connect` is scoped to one connection id, so several databases can be open at once. */
export interface Api {
  listProfiles(): Promise<Profile[]>;
  saveProfile(p: Profile): Promise<void>;
  deleteProfile(name: string): Promise<void>;
  connect(profile: Profile, password: string): Promise<string>;
  disconnect(conn: string): Promise<void>;
  runQuery(conn: string, sql: string, confirmed: boolean, pageSize: number): Promise<RunResult>;
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
  searchObjects(conn: string, text: string, owner: string | null, kinds: string[], limit: number): Promise<Hit[]>;
  previewComment(owner: string, table: string, column: string | null, text: string): Promise<string>;
  saveComment(conn: string, owner: string, table: string, column: string | null, text: string): Promise<string>;
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
    runQuery: (conn, sql, confirmed, pageSize) => invoke("run_query", { conn, sql, confirmed, pageSize }),
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
    searchObjects: (conn, text, owner, kinds, limit) => invoke("search_objects", { conn, text, owner, kinds, limit }),
    previewComment: (owner, table, column, text) => invoke("preview_comment", { owner, table, column, text }),
    saveComment: (conn, owner, table, column, text) => invoke("save_comment", { conn, owner, table, column, text }),
  };
}

/** Real backend inside Tauri; an in-browser demo (no database) otherwise. */
export const apiPromise: Promise<Api> = inTauri ? tauriApi() : Promise.resolve(mockApi);
export const isDemo = !inTauri;
