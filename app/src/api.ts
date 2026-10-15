import type { Completions, DbObject, Hit, Page, Profile, RefreshStats, RunResult, TableColumn, TableInsight, TableRef } from "./types";
import { mockApi } from "./mock";

export interface Api {
  listProfiles(): Promise<Profile[]>;
  saveProfile(p: Profile): Promise<void>;
  deleteProfile(name: string): Promise<void>;
  connect(profile: Profile, password: string): Promise<void>;
  disconnect(): Promise<void>;
  runQuery(sql: string, confirmed: boolean, pageSize: number): Promise<RunResult>;
  fetchMore(handle: number, pageSize: number): Promise<Page>;
  closeQuery(handle: number): Promise<void>;
  cancelQuery(): Promise<void>;
  commit(): Promise<void>;
  rollback(): Promise<void>;
  listSchemas(): Promise<string[]>;
  listObjects(owner: string): Promise<DbObject[]>;
  listColumns(owner: string, table: string): Promise<TableColumn[]>;
  refreshMetadata(owner: string, includeSource: boolean): Promise<RefreshStats>;
  tableInsight(owner: string, table: string): Promise<TableInsight>;
  complete(sql: string, cursor: number, owner: string): Promise<Completions>;
  references(sql: string): Promise<TableRef[]>;
  searchObjects(text: string, owner: string | null, kinds: string[], limit: number): Promise<Hit[]>;
  previewComment(owner: string, table: string, column: string | null, text: string): Promise<string>;
  saveComment(owner: string, table: string, column: string | null, text: string): Promise<string>;
}

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function tauriApi(): Promise<Api> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    listProfiles: () => invoke("list_profiles"),
    saveProfile: (profile) => invoke("save_profile", { profile }),
    deleteProfile: (name) => invoke("delete_profile", { name }),
    connect: (profile, password) => invoke("connect", { profile, password }),
    disconnect: () => invoke("disconnect"),
    runQuery: (sql, confirmed, pageSize) => invoke("run_query", { sql, confirmed, pageSize }),
    fetchMore: (handle, pageSize) => invoke("fetch_more", { handle, pageSize }),
    closeQuery: (handle) => invoke("close_query", { handle }),
    cancelQuery: () => invoke("cancel_query"),
    commit: () => invoke("commit"),
    rollback: () => invoke("rollback"),
    listSchemas: () => invoke("list_schemas"),
    listObjects: (owner) => invoke("list_objects", { owner }),
    listColumns: (owner, table) => invoke("list_columns", { owner, table }),
    refreshMetadata: (owner, includeSource) => invoke("refresh_metadata", { owner, includeSource }),
    tableInsight: (owner, table) => invoke("table_insight", { owner, table }),
    complete: (sql, cursor, owner) => invoke("complete", { sql, cursor, owner }),
    references: (sql) => invoke("references", { sql }),
    searchObjects: (text, owner, kinds, limit) => invoke("search_objects", { text, owner, kinds, limit }),
    previewComment: (owner, table, column, text) => invoke("preview_comment", { owner, table, column, text }),
    saveComment: (owner, table, column, text) => invoke("save_comment", { owner, table, column, text }),
  };
}

/** Real backend inside Tauri; an in-browser demo (no database) otherwise. */
export const apiPromise: Promise<Api> = inTauri ? tauriApi() : Promise.resolve(mockApi);
export const isDemo = !inTauri;
