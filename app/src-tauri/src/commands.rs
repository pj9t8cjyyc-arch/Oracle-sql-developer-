//! Thin IPC layer. All logic lives in `ora-core`; commands only translate
//! errors to strings and move blocking database calls off the UI thread.

use std::path::PathBuf;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use ora_core::catalog::{self, Dependencies, ObjectEntry, SequenceInfo};
use ora_core::comments;
use ora_core::complete::{self, Completions};
use ora_core::db::Database;
use ora_core::insight::{self, TableInsight};
use ora_core::meta::{self, MetaCache};
use ora_core::parse::{self, TableRef};
use ora_core::search::{self, Hit, SearchIndex, SearchQuery};
use ora_core::oracle_backend::OracleDb;
use ora_core::profile::ConnectionProfile;
use ora_core::schema;
use ora_core::session::{Page, QueryManager, RunOptions, RunResult};
use tauri::{AppHandle, Manager, State};

#[derive(Default)]
pub struct AppState {
    /// Several databases can be connected at once; each has an id.
    conns: Mutex<HashMap<String, Connected>>,
    next_id: AtomicU64,
    profiles_lock: Mutex<()>,
}

#[derive(Clone)]
struct Connected {
    db: Arc<dyn Database>,
    queries: Arc<QueryManager>,
    meta: Arc<RwLock<MetaCache>>,
    index: Arc<Mutex<SearchIndex>>,
}

/// Stable (FNV-1a) file-name fragment so each user@host/service gets its own index.
fn index_file_name(p: &ConnectionProfile) -> String {
    let id = format!("{}@{}:{}/{}", p.user, p.host, p.port, p.service).to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("index-{h:016x}.sqlite")
}

type CmdResult<T> = Result<T, String>;

fn current(state: &State<'_, AppState>, conn: &str) -> CmdResult<Connected> {
    state.conns.lock().unwrap().get(conn).cloned().ok_or_else(|| format!("not connected ({conn})"))
}

async fn blocking<T, F>(f: F) -> CmdResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ora_core::Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

fn profiles_path(app: &AppHandle) -> CmdResult<PathBuf> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("profiles.json"))
}

fn read_profiles(app: &AppHandle) -> CmdResult<Vec<ConnectionProfile>> {
    let path = profiles_path(app)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("corrupt profiles.json: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.to_string()),
    }
}

fn write_profiles(app: &AppHandle, list: &[ConnectionProfile]) -> CmdResult<()> {
    let path = profiles_path(app)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_profiles(app: AppHandle) -> CmdResult<Vec<ConnectionProfile>> {
    read_profiles(&app)
}

#[tauri::command]
pub fn save_profile(app: AppHandle, state: State<'_, AppState>, profile: ConnectionProfile) -> CmdResult<()> {
    profile.validate().map_err(|e| e.to_string())?;
    let _g = state.profiles_lock.lock().unwrap();
    let mut list = read_profiles(&app)?;
    list.retain(|p| p.name != profile.name);
    list.push(profile);
    write_profiles(&app, &list)
}

#[tauri::command]
pub fn delete_profile(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let _g = state.profiles_lock.lock().unwrap();
    let mut list = read_profiles(&app)?;
    list.retain(|p| p.name != name);
    write_profiles(&app, &list)
}

#[tauri::command]
pub async fn connect(app: AppHandle, state: State<'_, AppState>, profile: ConnectionProfile, password: String) -> CmdResult<String> {
    let env = profile.environment;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let index_path = dir.join(index_file_name(&profile));
    let (db, index) = blocking(move || {
        let db = OracleDb::connect(&profile, &password)?;
        Ok((db, SearchIndex::open(&index_path)?))
    })
    .await?;
    let db: Arc<dyn Database> = Arc::new(db);
    let id = format!("c{}", state.next_id.fetch_add(1, Ordering::Relaxed) + 1);
    state.conns.lock().unwrap().insert(
        id.clone(),
        Connected {
            queries: Arc::new(QueryManager::new(db.clone(), env)),
            db,
            meta: Arc::new(RwLock::new(MetaCache::default())),
            index: Arc::new(Mutex::new(index)),
        },
    );
    Ok(id)
}

#[tauri::command]
pub fn disconnect(state: State<'_, AppState>, conn: String) {
    // Dropping the last Arc closes the connection.
    state.conns.lock().unwrap().remove(&conn);
}

#[tauri::command]
pub async fn run_query(state: State<'_, AppState>, conn: String, sql: String, confirmed: bool, page_size: usize) -> CmdResult<RunResult> {
    let q = current(&state, &conn)?.queries;
    blocking(move || q.run(&sql, RunOptions { page_size, confirmed, ..Default::default() })).await
}

#[tauri::command]
pub async fn fetch_more(state: State<'_, AppState>, conn: String, handle: u64, page_size: usize) -> CmdResult<Page> {
    let q = current(&state, &conn)?.queries;
    blocking(move || q.fetch_more(handle, page_size)).await
}

#[tauri::command]
pub fn close_query(state: State<'_, AppState>, conn: String, handle: u64) -> CmdResult<()> {
    current(&state, &conn)?.queries.close(handle);
    Ok(())
}

#[tauri::command]
pub async fn cancel_query(state: State<'_, AppState>, conn: String) -> CmdResult<()> {
    let q = current(&state, &conn)?.queries;
    blocking(move || q.cancel()).await
}

#[tauri::command]
pub async fn commit(state: State<'_, AppState>, conn: String) -> CmdResult<()> {
    let q = current(&state, &conn)?.queries;
    blocking(move || q.commit()).await
}

#[tauri::command]
pub async fn rollback(state: State<'_, AppState>, conn: String) -> CmdResult<()> {
    let q = current(&state, &conn)?.queries;
    blocking(move || q.rollback()).await
}

#[tauri::command]
pub async fn list_schemas(state: State<'_, AppState>, conn: String) -> CmdResult<Vec<String>> {
    let db = current(&state, &conn)?.db;
    blocking(move || schema::schemas(db.as_ref())).await
}

#[derive(serde::Serialize)]
pub struct RefreshStats {
    tables: usize,
    columns: usize,
    indexed_docs: usize,
}

/// Load a schema's catalog for autocomplete and rebuild its search index.
/// `include_source` also indexes PL/SQL source lines (capped).
#[tauri::command]
pub async fn refresh_metadata(state: State<'_, AppState>, conn: String, owner: String, include_source: bool) -> CmdResult<RefreshStats> {
    let c = current(&state, &conn)?;
    blocking(move || {
        let schema = meta::load_schema(c.db.as_ref(), &owner)?;
        let mut docs = search::docs_from_meta(&schema);
        if include_source {
            docs.extend(search::load_source_docs(c.db.as_ref(), &owner, 300_000)?);
        }
        let stats = RefreshStats { tables: schema.tables.len(), columns: schema.tables.values().map(|t| t.columns.len()).sum(), indexed_docs: docs.len() };
        c.index.lock().unwrap().replace_owner(&owner, &docs)?;
        let owners = schema::schemas(c.db.as_ref())?;
        let mut cache = c.meta.write().unwrap();
        cache.owners = owners;
        cache.insert(schema);
        Ok(stats)
    })
    .await
}

#[tauri::command]
pub async fn table_insight(state: State<'_, AppState>, conn: String, owner: String, table: String) -> CmdResult<TableInsight> {
    let db = current(&state, &conn)?.db;
    blocking(move || insight::table_insight(db.as_ref(), &owner, &table)).await
}

/// In-memory and fast; `cursor` is a UTF-16 offset (CodeMirror coordinates).
#[tauri::command]
pub fn complete(state: State<'_, AppState>, conn: String, sql: String, cursor: usize, owner: String) -> CmdResult<Completions> {
    let c = current(&state, &conn)?;
    let cache = c.meta.read().unwrap();
    Ok(complete::complete(&cache, &owner, &sql, cursor))
}

#[tauri::command]
pub fn references(sql: String) -> Vec<TableRef> {
    parse::references(&sql)
}

#[tauri::command]
pub async fn search_objects(state: State<'_, AppState>, conn: String, text: String, owner: Option<String>, kinds: Vec<String>, limit: usize) -> CmdResult<Vec<Hit>> {
    let index = current(&state, &conn)?.index;
    blocking(move || index.lock().unwrap().search(&SearchQuery { text, owner, kinds, limit })).await
}

/// The exact SQL a comment save would run, for the confirmation dialog.
#[tauri::command]
pub fn preview_comment(owner: String, table: String, column: Option<String>, text: String) -> CmdResult<String> {
    comments::comment_sql(&owner, &table, column.as_deref(), &text).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_comment(state: State<'_, AppState>, conn: String, owner: String, table: String, column: Option<String>, text: String) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || comments::save_comment(db.as_ref(), &owner, &table, column.as_deref(), &text)).await
}

#[tauri::command]
pub async fn object_counts(state: State<'_, AppState>, conn: String, owner: String) -> CmdResult<BTreeMap<String, u64>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_counts(db.as_ref(), &owner)).await
}

#[tauri::command]
pub async fn list_objects(state: State<'_, AppState>, conn: String, owner: String, kind: String) -> CmdResult<Vec<ObjectEntry>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::list_objects(db.as_ref(), &owner, &kind)).await
}

#[tauri::command]
pub async fn object_source(state: State<'_, AppState>, conn: String, owner: String, name: String, kind: String) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_source(db.as_ref(), &owner, &name, &kind)).await
}

#[tauri::command]
pub async fn object_ddl(state: State<'_, AppState>, conn: String, owner: String, name: String, kind: String) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_ddl(db.as_ref(), &owner, &name, &kind)).await
}

#[tauri::command]
pub async fn sequence_info(state: State<'_, AppState>, conn: String, owner: String, name: String) -> CmdResult<Option<SequenceInfo>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::sequence_info(db.as_ref(), &owner, &name)).await
}

/// First rows of a table through the normal query session (so paging and cursor limits apply).
#[tauri::command]
pub async fn preview_table(state: State<'_, AppState>, conn: String, owner: String, table: String, limit: usize) -> CmdResult<RunResult> {
    let q = current(&state, &conn)?.queries;
    blocking(move || {
        let sql = catalog::preview_sql(&owner, &table, limit)?;
        q.run(&sql, RunOptions { page_size: limit.clamp(1, 1000), ..Default::default() })
    })
    .await
}

#[tauri::command]
pub async fn object_dependencies(state: State<'_, AppState>, conn: String, owner: String, name: String) -> CmdResult<Dependencies> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::dependencies(db.as_ref(), &owner, &name)).await
}
