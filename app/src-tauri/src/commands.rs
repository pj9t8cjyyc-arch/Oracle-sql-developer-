//! Thin IPC layer. All logic lives in `ora-core`; commands only translate
//! errors to strings and move blocking database calls off the UI thread.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ora_core::catalog::{self, Dependencies, ObjectEntry, SequenceInfo};
use ora_core::comments;
use ora_core::complete::{self, Completions};
use ora_core::db::Database;
use ora_core::history::{
    ExecRecord, ExecStatus, HistoryHit, HistoryQuery, HistoryStats, HistoryStore, Step, Worksheet,
    WorksheetFilter, WorksheetSave,
};
use ora_core::insight::{self, TableInsight};
use ora_core::meta::{self, MetaCache};
use ora_core::oracle_backend::OracleDb;
use ora_core::parse::{self, TableRef};
use ora_core::profile::ConnectionProfile;
use ora_core::schema;
use ora_core::search::{self, Hit, SearchIndex, SearchQuery};
use ora_core::session::{Page, QueryManager, RunOptions, RunResult};
use ora_core::sqltext::split_statements;
use tauri::{AppHandle, Manager, State};

pub struct AppState {
    /// Several databases can be connected at once; each has an id.
    conns: Mutex<HashMap<String, Connected>>,
    next_id: AtomicU64,
    profiles_lock: Mutex<()>,
    /// Local SQLite history + worksheets (survives restarts).
    history: Mutex<HistoryStore>,
    /// `false` when the history file could not be opened and an in-memory store is used instead.
    history_persistent: bool,
    history_enabled: AtomicBool,
    /// (connection id, cursor handle) -> execution id, so rows fetched later are added to the record.
    open_execs: Mutex<HashMap<(String, u64), i64>>,
    /// Running export/import jobs: id -> cancel flag.
    pub(crate) jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub(crate) job_seq: AtomicU64,
    /// Profiled import files, so editing the column mapping does not re-read the file.
    pub(crate) profiles: Mutex<HashMap<String, Arc<ora_core::import::profile::FileProfile>>>,
    /// Package analyses and dependency graphs per `connection|SCHEMA`; rebuilt on request.
    analyses: Mutex<HashMap<String, Arc<ora_core::packages::PackageAnalysis>>>,
    graphs: Mutex<HashMap<String, Arc<ora_core::graph::Graph>>>,
    /// Knowledge-base snapshots per `connection|SCHEMA`; the latest is also kept in the local SQLite file.
    kbs: Mutex<HashMap<String, Arc<ora_core::knowledge::Knowledge>>>,
    /// AI settings and the in-memory sign-in token (never persisted).
    pub(crate) ai: Arc<ora_core::ai::AiSession>,
    pub(crate) ai_calls: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl AppState {
    /// The local settings store, for the AI commands.
    pub(crate) fn history_for_ai(&self) -> &Mutex<HistoryStore> {
        &self.history
    }

    pub fn new(history: HistoryStore, persistent: bool) -> Self {
        let ai_cfg = history
            .setting_get("ai.config")
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str::<ora_core::ai::AiConfig>(&j).ok())
            .unwrap_or_default();
        Self {
            ai: Arc::new(ora_core::ai::AiSession::new(ai_cfg)),
            ai_calls: Mutex::default(),
            conns: Mutex::default(),
            next_id: AtomicU64::new(0),
            profiles_lock: Mutex::default(),
            history: Mutex::new(history),
            history_persistent: persistent,
            history_enabled: AtomicBool::new(true),
            open_execs: Mutex::default(),
            jobs: Mutex::default(),
            job_seq: AtomicU64::new(0),
            profiles: Mutex::default(),
            analyses: Mutex::default(),
            graphs: Mutex::default(),
            kbs: Mutex::default(),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone)]
pub(crate) struct Connected {
    pub(crate) db: Arc<dyn Database>,
    queries: Arc<QueryManager>,
    meta: Arc<RwLock<MetaCache>>,
    index: Arc<Mutex<SearchIndex>>,
    /// Stable identity of user@host:port/service: the same across sessions, unlike the connection id.
    key: String,
    name: String,
    /// Opens another session to the same database for exports and imports, so a long job never
    /// blocks the worksheets and has its own transaction. The password is therefore kept in
    /// memory for as long as the connection is open (never written to disk).
    pub(crate) opener: Arc<dyn Fn() -> ora_core::Result<Arc<dyn Database>> + Send + Sync>,
}

/// Stable (FNV-1a) identity of user@host:port/service.
fn index_file_name(p: &ConnectionProfile) -> String {
    format!("index-{}.sqlite", p.key())
}

#[derive(serde::Serialize)]
pub struct ConnInfo {
    id: String,
    key: String,
}

type CmdResult<T> = Result<T, String>;

pub(crate) fn current(state: &State<'_, AppState>, conn: &str) -> CmdResult<Connected> {
    state
        .conns
        .lock()
        .unwrap()
        .get(conn)
        .cloned()
        .ok_or_else(|| format!("not connected ({conn})"))
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
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(list).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_profiles(app: AppHandle) -> CmdResult<Vec<ConnectionProfile>> {
    read_profiles(&app)
}

#[tauri::command]
pub fn save_profile(
    app: AppHandle,
    state: State<'_, AppState>,
    profile: ConnectionProfile,
) -> CmdResult<()> {
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
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: String,
) -> CmdResult<ConnInfo> {
    let env = profile.environment;
    let (key, name) = (profile.key(), profile.name.clone());
    let opener: Arc<dyn Fn() -> ora_core::Result<Arc<dyn Database>> + Send + Sync> = {
        let (p, pw) = (profile.clone(), password.clone());
        Arc::new(move || Ok(Arc::new(OracleDb::connect(&p, &pw)?) as Arc<dyn Database>))
    };
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
            key: key.clone(),
            name,
            opener,
        },
    );
    Ok(ConnInfo { id, key })
}

#[tauri::command]
pub fn disconnect(state: State<'_, AppState>, conn: String) {
    // Dropping the last Arc closes the connection.
    state.conns.lock().unwrap().remove(&conn);
    state
        .open_execs
        .lock()
        .unwrap()
        .retain(|(c, _), _| c != &conn);
}

#[tauri::command]
pub async fn run_query(
    state: State<'_, AppState>,
    conn: String,
    sql: String,
    confirmed: bool,
    page_size: usize,
    owner: String,
    worksheet: Option<String>,
) -> CmdResult<RunResult> {
    let c = current(&state, &conn)?;
    let q = c.queries.clone();
    let text = sql.clone();
    let started_at = now_ms();
    let t0 = Instant::now();
    let raw = tauri::async_runtime::spawn_blocking(move || {
        q.run(
            &text,
            RunOptions {
                page_size,
                confirmed,
                ..Default::default()
            },
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    let duration_ms = t0.elapsed().as_millis() as i64;

    // Record what actually ran. Statements that were refused or are waiting for
    // confirmation never reached the database, so they are not history.
    let (status, rows, error) = match &raw {
        Ok(RunResult::Rows { rows, .. }) => (Some(ExecStatus::Ok), Some(rows.len() as i64), None),
        Ok(RunResult::Affected { count, .. }) => (Some(ExecStatus::Ok), Some(*count as i64), None),
        Ok(RunResult::Done { .. }) => (Some(ExecStatus::Ok), None, None),
        Ok(RunResult::NeedsConfirmation { .. }) => (None, None, None),
        Err(ora_core::Error::Database(m)) => (
            Some(if m.contains("ORA-01013") {
                ExecStatus::Cancelled
            } else {
                ExecStatus::Error
            }),
            None,
            Some(m.clone()),
        ),
        Err(_) => (None, None, None),
    };
    if let (Some(status), true) = (status, state.history_enabled.load(Ordering::Relaxed)) {
        let rec = ExecRecord {
            sql: sql.clone(),
            conn_key: c.key.clone(),
            conn_name: c.name.clone(),
            owner,
            worksheet,
            started_at,
            duration_ms,
            rows,
            status,
            error,
        };
        match state.history.lock().unwrap().record_execution(&rec) {
            Ok(id) => {
                if let Ok(RunResult::Rows {
                    handle,
                    done: false,
                    ..
                }) = &raw
                {
                    state
                        .open_execs
                        .lock()
                        .unwrap()
                        .insert((conn.clone(), *handle), id);
                }
            }
            Err(e) => eprintln!("history: could not record execution: {e}"),
        }
    }
    raw.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn fetch_more(
    state: State<'_, AppState>,
    conn: String,
    handle: u64,
    page_size: usize,
) -> CmdResult<Page> {
    let q = current(&state, &conn)?.queries;
    let page = blocking(move || q.fetch_more(handle, page_size)).await?;
    let key = (conn, handle);
    let exec = {
        let mut m = state.open_execs.lock().unwrap();
        if page.done {
            m.remove(&key)
        } else {
            m.get(&key).copied()
        }
    };
    if let Some(id) = exec {
        if let Err(e) = state
            .history
            .lock()
            .unwrap()
            .add_rows(id, page.rows.len() as i64)
        {
            eprintln!("history: could not update row count: {e}");
        }
    }
    Ok(page)
}

#[tauri::command]
pub fn close_query(state: State<'_, AppState>, conn: String, handle: u64) -> CmdResult<()> {
    current(&state, &conn)?.queries.close(handle);
    state.open_execs.lock().unwrap().remove(&(conn, handle));
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
pub async fn refresh_metadata(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    include_source: bool,
) -> CmdResult<RefreshStats> {
    let c = current(&state, &conn)?;
    blocking(move || {
        let schema = meta::load_schema(c.db.as_ref(), &owner)?;
        let mut docs = search::docs_from_meta(&schema);
        if include_source {
            docs.extend(search::load_source_docs(c.db.as_ref(), &owner, 300_000)?);
        }
        let stats = RefreshStats {
            tables: schema.tables.len(),
            columns: schema.tables.values().map(|t| t.columns.len()).sum(),
            indexed_docs: docs.len(),
        };
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
pub async fn table_insight(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    table: String,
) -> CmdResult<TableInsight> {
    let db = current(&state, &conn)?.db;
    blocking(move || insight::table_insight(db.as_ref(), &owner, &table)).await
}

/// In-memory and fast; `cursor` is a UTF-16 offset (CodeMirror coordinates).
#[tauri::command]
pub fn complete(
    state: State<'_, AppState>,
    conn: String,
    sql: String,
    cursor: usize,
    owner: String,
) -> CmdResult<Completions> {
    let c = current(&state, &conn)?;
    let cache = c.meta.read().unwrap();
    Ok(complete::complete(&cache, &owner, &sql, cursor))
}

#[tauri::command]
pub fn references(sql: String) -> Vec<TableRef> {
    parse::references(&sql)
}

#[tauri::command]
pub async fn search_objects(
    state: State<'_, AppState>,
    conn: String,
    text: String,
    owner: Option<String>,
    kinds: Vec<String>,
    limit: usize,
) -> CmdResult<Vec<Hit>> {
    let index = current(&state, &conn)?.index;
    blocking(move || {
        index.lock().unwrap().search(&SearchQuery {
            text,
            owner,
            kinds,
            limit,
        })
    })
    .await
}

/// The exact SQL a comment save would run, for the confirmation dialog.
#[tauri::command]
pub fn preview_comment(
    owner: String,
    table: String,
    column: Option<String>,
    text: String,
) -> CmdResult<String> {
    comments::comment_sql(&owner, &table, column.as_deref(), &text).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_comment(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    table: String,
    column: Option<String>,
    text: String,
) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || comments::save_comment(db.as_ref(), &owner, &table, column.as_deref(), &text))
        .await
}

#[tauri::command]
pub async fn object_counts(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
) -> CmdResult<BTreeMap<String, u64>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_counts(db.as_ref(), &owner)).await
}

#[tauri::command]
pub async fn list_objects(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    kind: String,
) -> CmdResult<Vec<ObjectEntry>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::list_objects(db.as_ref(), &owner, &kind)).await
}

#[tauri::command]
pub async fn object_source(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    name: String,
    kind: String,
) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_source(db.as_ref(), &owner, &name, &kind)).await
}

#[tauri::command]
pub async fn object_ddl(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    name: String,
    kind: String,
) -> CmdResult<String> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::object_ddl(db.as_ref(), &owner, &name, &kind)).await
}

#[tauri::command]
pub async fn sequence_info(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    name: String,
) -> CmdResult<Option<SequenceInfo>> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::sequence_info(db.as_ref(), &owner, &name)).await
}

/// First rows of a table through the normal query session (so paging and cursor limits apply).
#[tauri::command]
pub async fn preview_table(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    table: String,
    limit: usize,
) -> CmdResult<RunResult> {
    let q = current(&state, &conn)?.queries;
    blocking(move || {
        let sql = catalog::preview_sql(&owner, &table, limit)?;
        q.run(
            &sql,
            RunOptions {
                page_size: limit.clamp(1, 1000),
                ..Default::default()
            },
        )
    })
    .await
}

#[tauri::command]
pub async fn object_dependencies(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    name: String,
) -> CmdResult<Dependencies> {
    let db = current(&state, &conn)?.db;
    blocking(move || catalog::dependencies(db.as_ref(), &owner, &name)).await
}

// ------------------------------------------------------------ history + worksheets

fn history<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut HistoryStore) -> ora_core::Result<T>,
) -> CmdResult<T> {
    f(&mut state.history.lock().unwrap()).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct HistoryStatus {
    enabled: bool,
    /// `false`: the history file could not be opened, so history only lasts until the app closes.
    persistent: bool,
    stats: HistoryStats,
}

#[tauri::command]
pub fn history_status(state: State<'_, AppState>) -> CmdResult<HistoryStatus> {
    let stats = history(&state, |h| h.stats())?;
    Ok(HistoryStatus {
        enabled: state.history_enabled.load(Ordering::Relaxed),
        persistent: state.history_persistent,
        stats,
    })
}

#[tauri::command]
pub fn history_set_enabled(state: State<'_, AppState>, enabled: bool) {
    state.history_enabled.store(enabled, Ordering::Relaxed);
}

#[tauri::command]
pub fn history_search(
    state: State<'_, AppState>,
    query: HistoryQuery,
) -> CmdResult<Vec<HistoryHit>> {
    history(&state, |h| h.search(&query, now_ms()))
}

#[tauri::command]
pub fn history_suggest(
    state: State<'_, AppState>,
    typed: String,
    conn_key: Option<String>,
    limit: usize,
) -> CmdResult<Vec<HistoryHit>> {
    history(&state, |h| {
        h.suggest(&typed, conn_key.as_deref(), limit, now_ms())
    })
}

#[tauri::command]
pub fn history_pin(state: State<'_, AppState>, hash: String, pinned: bool) -> CmdResult<()> {
    history(&state, |h| h.set_pinned(&hash, pinned))
}

#[tauri::command]
pub fn history_forget(state: State<'_, AppState>, hash: String) -> CmdResult<()> {
    history(&state, |h| h.forget_query(&hash))
}

#[tauri::command]
pub fn history_purge(state: State<'_, AppState>, before_ms: i64) -> CmdResult<usize> {
    history(&state, |h| h.purge_before(before_ms))
}

#[tauri::command]
pub fn ws_save(state: State<'_, AppState>, ws: WorksheetSave) -> CmdResult<Option<Worksheet>> {
    history(&state, |h| h.save_worksheet(&ws, now_ms()))
}

#[tauri::command]
pub fn ws_close(state: State<'_, AppState>, uuid: String) -> CmdResult<()> {
    history(&state, |h| h.close_worksheet(&uuid, now_ms()))
}

#[tauri::command]
pub fn ws_pin(state: State<'_, AppState>, uuid: String, pinned: bool) -> CmdResult<()> {
    history(&state, |h| h.set_worksheet_pinned(&uuid, pinned))
}

#[tauri::command]
pub fn ws_delete(state: State<'_, AppState>, uuid: String) -> CmdResult<()> {
    history(&state, |h| h.delete_worksheet(&uuid))
}

#[tauri::command]
pub fn ws_list(state: State<'_, AppState>, filter: WorksheetFilter) -> CmdResult<Vec<Worksheet>> {
    history(&state, |h| h.list_worksheets(&filter))
}

#[tauri::command]
pub fn ws_get(state: State<'_, AppState>, uuid: String) -> CmdResult<Option<Worksheet>> {
    history(&state, |h| h.get_worksheet(&uuid))
}

#[tauri::command]
pub fn ws_steps(state: State<'_, AppState>, uuid: String) -> CmdResult<Vec<Step>> {
    history(&state, |h| h.worksheet_steps(&uuid))
}

#[derive(serde::Serialize)]
pub struct StatementRange {
    /// UTF-16 offsets (what the editor uses).
    start: usize,
    end: usize,
    kind: ora_core::guard::StatementKind,
    text: String,
}

/// Split a script into statements (`;`, or `/` after PL/SQL blocks) so the editor can
/// run the one under the cursor or all of them in order.
#[tauri::command]
pub fn statements(sql: String) -> Vec<StatementRange> {
    let mut utf16 = Vec::with_capacity(sql.chars().count() + 1);
    let mut units = 0;
    for c in sql.chars() {
        utf16.push(units);
        units += c.len_utf16();
    }
    utf16.push(units);
    split_statements(&sql)
        .into_iter()
        .map(|s| StatementRange {
            start: utf16[s.start],
            end: utf16[s.end],
            kind: s.kind,
            text: s.text,
        })
        .collect()
}

// ------------------------------------------------------------ packages + linkage graph

fn cache_key(conn: &str, owner: &str) -> String {
    format!("{conn}|{}", owner.to_uppercase())
}

/// Every package of a schema: members, signatures, calls, tables read/written, unreferenced members.
/// Cached until `refresh` is true (it reads all package bodies).
#[tauri::command]
pub async fn package_analysis(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    refresh: bool,
) -> CmdResult<Arc<ora_core::packages::PackageAnalysis>> {
    let key = cache_key(&conn, &owner);
    if !refresh {
        if let Some(a) = state.analyses.lock().unwrap().get(&key) {
            return Ok(a.clone());
        }
    }
    let db = current(&state, &conn)?.db;
    let a =
        Arc::new(blocking(move || ora_core::packages::analyze_schema(db.as_ref(), &owner)).await?);
    state.analyses.lock().unwrap().insert(key, a.clone());
    Ok(a)
}

async fn graph_for(
    state: &State<'_, AppState>,
    conn: &str,
    owner: &str,
    refresh: bool,
) -> CmdResult<Arc<ora_core::graph::Graph>> {
    let key = cache_key(conn, owner);
    if !refresh {
        if let Some(g) = state.graphs.lock().unwrap().get(&key) {
            return Ok(g.clone());
        }
    }
    let db = current(state, conn)?.db;
    let owner = owner.to_string();
    let g = Arc::new(blocking(move || ora_core::graph::load(db.as_ref(), &owner)).await?);
    state.graphs.lock().unwrap().insert(key, g.clone());
    Ok(g)
}

#[tauri::command]
pub async fn graph_info(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    refresh: bool,
) -> CmdResult<ora_core::graph::GraphInfo> {
    Ok(graph_for(&state, &conn, &owner, refresh).await?.info())
}

#[tauri::command]
pub async fn graph_search(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    query: String,
) -> CmdResult<Vec<ora_core::graph::GNode>> {
    Ok(graph_for(&state, &conn, &owner, false)
        .await?
        .search(&query, 30))
}

#[tauri::command]
pub async fn graph_focus(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    focus: ora_core::graph::Focus,
) -> CmdResult<ora_core::graph::Subgraph> {
    graph_for(&state, &conn, &owner, false)
        .await?
        .focus(&focus)
        .ok_or_else(|| "that object is not in the graph".to_string())
}

// ------------------------------------------------------------ knowledge base + markdown export

/// Reads the whole schema (objects, columns, keys, indexes, triggers, packages, jobs, grants) into a
/// linked knowledge base with evidence, stores it in the local SQLite file and makes the graph use it.
#[tauri::command]
pub async fn kb_build(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
) -> CmdResult<ora_core::knowledge::KbSummary> {
    let c = current(&state, &conn)?;
    let db = c.db.clone();
    let o = owner.clone();
    let kb =
        blocking(move || ora_core::knowledge::collect_schema(db.as_ref(), &o, now_ms())).await?;
    let json = serde_json::to_string(&kb).map_err(|e| e.to_string())?;
    history(&state, |h| h.kb_save(&c.key, &owner, kb.taken_at, &json))?;
    let key = cache_key(&conn, &owner);
    let summary = kb.summary();
    state
        .graphs
        .lock()
        .unwrap()
        .insert(key.clone(), Arc::new(kb.graph()));
    state.kbs.lock().unwrap().insert(key, Arc::new(kb));
    Ok(summary)
}

pub(crate) fn kb_for(
    state: &State<'_, AppState>,
    conn: &str,
    owner: &str,
) -> CmdResult<Option<Arc<ora_core::knowledge::Knowledge>>> {
    let key = cache_key(conn, owner);
    if let Some(k) = state.kbs.lock().unwrap().get(&key) {
        return Ok(Some(k.clone()));
    }
    let c = current(state, conn)?;
    let Some((_, json)) = history(state, |h| h.kb_load(&c.key, owner))? else {
        return Ok(None);
    };
    let kb: ora_core::knowledge::Knowledge =
        serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let kb = Arc::new(kb);
    state
        .graphs
        .lock()
        .unwrap()
        .insert(key.clone(), Arc::new(kb.graph()));
    state.kbs.lock().unwrap().insert(key, kb.clone());
    Ok(Some(kb))
}

#[tauri::command]
pub async fn kb_status(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
) -> CmdResult<Option<ora_core::knowledge::KbSummary>> {
    Ok(kb_for(&state, &conn, &owner)?.map(|k| k.summary()))
}

#[derive(serde::Serialize)]
pub struct KbPreview {
    markdown: String,
    files: Vec<String>,
    tokens_estimate: usize,
    truncated: bool,
    missing: Vec<String>,
    /// The preview text was cut (the export is complete).
    preview_cut: bool,
}

/// Source and DDL are read on a second session so a long export never blocks a worksheet.
fn build_bundle(
    state: &State<'_, AppState>,
    conn: &str,
    owner: &str,
    options: ora_core::dossier::DossierOptions,
) -> CmdResult<impl FnOnce() -> ora_core::Result<ora_core::dossier::Bundle> + Send + 'static> {
    let kb = kb_for(state, conn, owner)?
        .ok_or_else(|| "Build the knowledge base for this schema first.".to_string())?;
    let c = current(state, conn)?;
    Ok(move || {
        let db = (c.opener)().unwrap_or_else(|_| c.db.clone());
        ora_core::dossier::build(&kb, &ora_core::dossier::DbFetch(db.as_ref()), &options)
    })
}

#[tauri::command]
pub async fn kb_preview(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    options: ora_core::dossier::DossierOptions,
) -> CmdResult<KbPreview> {
    let job = build_bundle(&state, &conn, &owner, options)?;
    let b = blocking(job).await?;
    const CAP: usize = 300_000;
    let md = b.combined();
    let cut = md.len() > CAP;
    let markdown = if cut {
        let mut end = CAP;
        while !md.is_char_boundary(end) {
            end -= 1;
        }
        md[..end].to_string()
    } else {
        md
    };
    Ok(KbPreview {
        markdown,
        files: b.files.iter().map(|f| f.path.clone()).collect(),
        tokens_estimate: b.tokens_estimate,
        truncated: b.truncated,
        missing: b.missing,
        preview_cut: cut,
    })
}

#[derive(serde::Serialize)]
pub struct KbExported {
    path: String,
    bytes: u64,
    files: usize,
    tokens_estimate: usize,
}

/// Writes one `.md` file, or a `.zip` with one file per object, an index, `graph.json` and `manifest.json`.
#[tauri::command]
pub async fn kb_export(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    options: ora_core::dossier::DossierOptions,
    path: String,
    zip: bool,
) -> CmdResult<KbExported> {
    let job = build_bundle(&state, &conn, &owner, options)?;
    blocking(move || {
        let b = job()?;
        let bytes = {
            let p = std::path::Path::new(&path);
            if zip {
                b.write_zip(p)?;
            } else {
                b.write_combined(p)?;
            }
            std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
        };
        Ok(KbExported {
            path,
            bytes,
            files: b.files.len(),
            tokens_estimate: b.tokens_estimate,
        })
    })
    .await
}

// ------------------------------------------------------------ connection helpers

#[derive(serde::Serialize)]
pub struct ConnTest {
    ok: bool,
    error: Option<String>,
    details: Option<ora_core::conninfo::ConnDetails>,
    elapsed_ms: u64,
    /// Version of the Oracle client library in use.
    client: Option<String>,
}

/// Connects once, reads the database version and a few facts, and disconnects. Never changes anything.
#[tauri::command]
pub async fn test_connection(profile: ConnectionProfile, password: String) -> CmdResult<ConnTest> {
    tauri::async_runtime::spawn_blocking(move || {
        let t = std::time::Instant::now();
        let client = ora_core::oracle_backend::client_version();
        Ok(match OracleDb::connect(&profile, &password) {
            Ok(db) => {
                let details = ora_core::conninfo::details(&db);
                ConnTest {
                    ok: details.is_ok(),
                    error: details.as_ref().err().map(|e| e.to_string()),
                    details: details.ok(),
                    elapsed_ms: t.elapsed().as_millis() as u64,
                    client: client.ok(),
                }
            }
            Err(e) => ConnTest {
                ok: false,
                error: Some(e.to_string()),
                details: None,
                elapsed_ms: t.elapsed().as_millis() as u64,
                client: client.ok(),
            },
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
pub struct TnsSummary {
    names: Vec<String>,
    host: Option<String>,
    port: Option<String>,
    service: Option<String>,
    tls: bool,
}

#[derive(serde::Serialize)]
pub struct TnsList {
    file: String,
    entries: Vec<TnsSummary>,
}

/// The aliases in a `tnsnames.ora` (folder or file; empty = `TNS_ADMIN` / `ORACLE_HOME`).
#[tauri::command]
pub fn tns_aliases(path: String) -> CmdResult<TnsList> {
    let (file, entries) = ora_core::tns::load(&path).map_err(|e| e.to_string())?;
    Ok(TnsList {
        file: file.display().to_string(),
        entries: entries
            .into_iter()
            .map(|e| TnsSummary {
                names: e.names,
                host: e.host,
                port: e.port,
                service: e.service,
                tls: e.tls,
            })
            .collect(),
    })
}

#[derive(serde::Serialize)]
pub struct ClientStatus {
    ok: bool,
    version: Option<String>,
    error: Option<String>,
}

/// Is Oracle Instant Client installed and loadable?
#[tauri::command]
pub async fn client_status() -> CmdResult<ClientStatus> {
    tauri::async_runtime::spawn_blocking(|| match ora_core::oracle_backend::client_version() {
        Ok(v) => ClientStatus {
            ok: true,
            version: Some(v),
            error: None,
        },
        Err(e) => ClientStatus {
            ok: false,
            version: None,
            error: Some(e),
        },
    })
    .await
    .map_err(|e| e.to_string())
}

// ------------------------------------------------------------ explain plan

/// Explains a statement without running it; the plan rows are removed from `PLAN_TABLE` afterwards.
#[tauri::command]
pub async fn explain_plan(
    state: State<'_, AppState>,
    conn: String,
    sql: String,
) -> CmdResult<ora_core::plan::Plan> {
    let c = current(&state, &conn)?;
    let db = c.db.clone();
    blocking(move || ora_core::plan::explain(db.as_ref(), &sql)).await
}
