//! Export and import commands. Long work runs on a background thread over its own
//! database session and reports through `job-progress` / `job-done` events.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ora_core::db::{collect, Database, Outcome};
use ora_core::export::{self, ExportOptions, Format, Purpose, Recommendation};
use ora_core::guard::{analyze, StatementKind};
use ora_core::import::plan::{self, ImportOptions, MappingPlan, PlanInput};
use ora_core::import::profile::{self, FileProfile};
use ora_core::import::source::{open_source, ReadOptions};
use ora_core::profile::Environment;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{current, AppState};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize, Clone)]
struct JobProgress {
    id: String,
    kind: &'static str,
    rows: u64,
    bytes: u64,
    rejected: u64,
    elapsed_ms: u64,
}

#[derive(Serialize, Clone)]
struct JobDone {
    id: String,
    kind: &'static str,
    ok: bool,
    cancelled: bool,
    error: Option<String>,
    report: serde_json::Value,
}

struct JobCtx {
    id: String,
    kind: &'static str,
    app: AppHandle,
    cancel: Arc<AtomicBool>,
}

impl JobCtx {
    fn progress(&self, rows: u64, bytes: u64, rejected: u64, elapsed_ms: u64) {
        let _ = self.app.emit(
            "job-progress",
            JobProgress {
                id: self.id.clone(),
                kind: self.kind,
                rows,
                bytes,
                rejected,
                elapsed_ms,
            },
        );
    }
}

/// Run `work` on its own thread; its result becomes the `job-done` event.
fn spawn_job(
    app: AppHandle,
    state: &State<'_, AppState>,
    kind: &'static str,
    work: impl FnOnce(&JobCtx) -> Result<(serde_json::Value, bool), String> + Send + 'static,
) -> String {
    let id = format!("job{}", state.job_seq.fetch_add(1, Ordering::Relaxed) + 1);
    let cancel = Arc::new(AtomicBool::new(false));
    state
        .jobs
        .lock()
        .unwrap()
        .insert(id.clone(), cancel.clone());
    let ctx = JobCtx {
        id: id.clone(),
        kind,
        app: app.clone(),
        cancel,
    };
    let spawned = std::thread::Builder::new()
        .name(format!("ora-{kind}"))
        .spawn(move || {
            let result = work(&ctx);
            let done = match result {
                Ok((report, cancelled)) => JobDone {
                    id: ctx.id.clone(),
                    kind,
                    ok: !cancelled,
                    cancelled,
                    error: None,
                    report,
                },
                Err(e) => JobDone {
                    id: ctx.id.clone(),
                    kind,
                    ok: false,
                    cancelled: false,
                    error: Some(e),
                    report: serde_json::Value::Null,
                },
            };
            ctx.app
                .state::<AppState>()
                .jobs
                .lock()
                .unwrap()
                .remove(&ctx.id);
            let _ = ctx.app.emit("job-done", done);
        });
    if let Err(e) = spawned {
        state.jobs.lock().unwrap().remove(&id);
        let _ = app.emit(
            "job-done",
            JobDone {
                id: id.clone(),
                kind,
                ok: false,
                cancelled: false,
                error: Some(format!("could not start the job: {e}")),
                report: serde_json::Value::Null,
            },
        );
    }
    id
}

#[tauri::command]
pub fn job_cancel(state: State<'_, AppState>, id: String) {
    if let Some(flag) = state.jobs.lock().unwrap().get(&id) {
        flag.store(true, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------------------- export

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExportSource {
    Query {
        sql: String,
    },
    Table {
        owner: String,
        table: String,
        where_clause: Option<String>,
    },
}

impl ExportSource {
    fn sql(&self, row_limit: Option<u64>) -> ora_core::Result<String> {
        let base = match self {
            ExportSource::Query { sql } => sql.trim().trim_end_matches(';').trim().to_string(),
            ExportSource::Table {
                owner,
                table,
                where_clause,
            } => {
                let mut s = format!(
                    "SELECT * FROM {}.{}",
                    ora_core::comments::quote_ident(owner)?,
                    ora_core::comments::quote_ident(table)?
                );
                if let Some(w) = where_clause
                    .as_deref()
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                {
                    s.push_str(&format!(" WHERE {w}"));
                }
                s
            }
        };
        Ok(match row_limit {
            Some(n) if n > 0 => format!("SELECT * FROM ({base}) WHERE ROWNUM <= {n}"),
            _ => base,
        })
    }
}

#[derive(Serialize)]
pub struct RecommendationInfo {
    est_rows: Option<u64>,
    /// Row counts come from optimizer statistics and may be out of date.
    from_statistics: bool,
    recommendation: Recommendation,
}

#[tauri::command]
pub async fn export_recommend(
    state: State<'_, AppState>,
    conn: String,
    source: ExportSource,
    purpose: Purpose,
) -> CmdResult<RecommendationInfo> {
    let db = current(&state, &conn)?.db;
    tauri::async_runtime::spawn_blocking(move || {
        let est = match &source {
            ExportSource::Table { owner, table, where_clause } if where_clause.as_deref().map(str::trim).unwrap_or("").is_empty() => collect(db.as_ref(), "SELECT /*exp:rows*/ num_rows FROM all_tables WHERE owner = :1 AND table_name = :2", &[owner.clone(), table.clone()])
                .ok()
                .and_then(|(_, r)| r.first().cloned())
                .and_then(|r| r.first().cloned().flatten())
                .and_then(|v| v.parse::<f64>().ok())
                .map(|v| v as u64),
            _ => None,
        };
        let recommendation = export::recommend(est, purpose);
        RecommendationInfo { est_rows: est, from_statistics: est.is_some(), recommendation }
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportRequest {
    pub source: ExportSource,
    /// Where to write; the extension is chosen from the format.
    pub path: String,
    pub options: ExportOptions,
    pub row_limit: Option<u64>,
    pub overwrite: bool,
}

/// Dates, timestamps and numbers must come back in fixed formats whatever the user's session
/// uses, or a German locale would write `1.234,5` and a US one `01-JAN-20`.
const EXPORT_NLS: &str = "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM' NLS_NUMERIC_CHARACTERS = '.,'";

fn check_output_path(req: &ExportRequest) -> Result<PathBuf, String> {
    let base = PathBuf::from(&req.path);
    if !base.is_absolute() {
        return Err("the output path must be absolute".into());
    }
    let dir = base.parent().ok_or("the output path has no folder")?;
    if !dir.is_dir() {
        return Err(format!("the folder {} does not exist", dir.display()));
    }
    let first = export::first_output_path(&base, &req.options);
    if first.exists() && !req.overwrite {
        return Err(format!("{} already exists", first.display()));
    }
    Ok(base)
}

#[tauri::command]
pub fn export_start(
    app: AppHandle,
    state: State<'_, AppState>,
    conn: String,
    request: ExportRequest,
) -> CmdResult<String> {
    let c = current(&state, &conn)?;
    let base = check_output_path(&request)?;
    let mut opts = request.options.clone();
    if let (ExportSource::Table { owner, table, .. }, Format::SqlInsert) =
        (&request.source, opts.format)
    {
        opts.insert_table = Some(format!(
            "{}.{}",
            ora_core::comments::quote_ident(owner).map_err(|e| e.to_string())?,
            ora_core::comments::quote_ident(table).map_err(|e| e.to_string())?
        ));
    }
    // Only a plain SELECT may be exported: the statement runs on a read-only session, but refuse anything else up front.
    let sql = request
        .source
        .sql(request.row_limit)
        .map_err(|e| e.to_string())?;
    let a = analyze(&sql, Environment::Dev);
    if a.kind != StatementKind::Select || a.blocked.is_some() || !a.read_only {
        return Err(a
            .blocked
            .unwrap_or_else(|| "only a single SELECT statement can be exported".into()));
    }
    let opener = c.opener.clone();
    let sql = a.sql;
    Ok(spawn_job(app, &state, "export", move |ctx| {
        let db = opener().map_err(|e| e.to_string())?;
        db.run(EXPORT_NLS, &[], 1).map_err(|e| e.to_string())?;
        let _ = db.run("SET TRANSACTION READ ONLY", &[], 1); // best effort: stops side effects from functions in the query
        let mut cursor = match db.run(&sql, &[], 10_000).map_err(|e| e.to_string())? {
            Outcome::Rows(c) => c,
            _ => return Err("the statement returned no rows to export".into()),
        };
        let report = export::export(cursor.as_mut(), &base, &opts, &ctx.cancel, &mut |p| {
            ctx.progress(p.rows, p.bytes, 0, p.elapsed_ms)
        })
        .map_err(|e| e.to_string())?;
        let cancelled = report.cancelled;
        Ok((serde_json::to_value(report).unwrap_or_default(), cancelled))
    }))
}

// ------------------------------------------------------------------------- import

#[derive(Serialize)]
pub struct ProfileInfo {
    id: String,
    profile: FileProfile,
}

#[tauri::command]
pub async fn import_profile(
    app: AppHandle,
    path: String,
    read: ReadOptions,
) -> CmdResult<ProfileInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        if !p.is_file() {
            return Err(format!("{path} is not a file"));
        }
        let mut src = open_source(&p, &read).map_err(|e| e.to_string())?;
        let prof = profile::profile_source(src.as_mut(), 100_000).map_err(|e| e.to_string())?;
        let state = app.state::<AppState>();
        let id = format!("p{}", state.job_seq.fetch_add(1, Ordering::Relaxed) + 1);
        let mut cache = state.profiles.lock().unwrap();
        if cache.len() > 20 {
            cache.clear();
        }
        cache.insert(id.clone(), Arc::new(prof.clone()));
        Ok(ProfileInfo { id, profile: prof })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn cached_profile(state: &State<'_, AppState>, id: &str) -> CmdResult<Arc<FileProfile>> {
    state
        .profiles
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| "the file profile expired: choose the file again".to_string())
}

#[tauri::command]
pub async fn import_plan(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    table: String,
    profile_id: String,
    input: PlanInput,
) -> CmdResult<MappingPlan> {
    let db = current(&state, &conn)?.db;
    let prof = cached_profile(&state, &profile_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let targets = plan::load_targets(db.as_ref(), &owner, &table).map_err(|e| e.to_string())?;
        Ok(plan::plan_mapping(&owner, &table, &prof, &targets, &input))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn import_ddl(
    state: State<'_, AppState>,
    profile_id: String,
    owner: String,
    table: String,
) -> CmdResult<String> {
    let prof = cached_profile(&state, &profile_id)?;
    profile::create_table_ddl(&prof, &owner, &table).map_err(|e| e.to_string())
}

/// Run a `CREATE TABLE` the user has reviewed, on their own session. Nothing else is accepted.
#[tauri::command]
pub async fn import_create_table(
    state: State<'_, AppState>,
    conn: String,
    ddl: String,
) -> CmdResult<()> {
    let db = current(&state, &conn)?.db;
    let a = analyze(&ddl, Environment::Dev);
    let upper = a.sql.to_uppercase();
    if a.kind != StatementKind::Ddl
        || a.blocked.is_some()
        || !upper.starts_with("CREATE TABLE ")
        || a.sql.contains(';')
    {
        return Err("only a single CREATE TABLE statement can be run here".into());
    }
    let sql = a.sql;
    tauri::async_runtime::spawn_blocking(move || {
        db.run(&sql, &[], 1).map(|_| ()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn import_validate_start(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    read: ReadOptions,
    plan: MappingPlan,
    trim: bool,
) -> CmdResult<String> {
    Ok(spawn_job(app, &state, "validate", move |ctx| {
        let mut src = open_source(Path::new(&path), &read).map_err(|e| e.to_string())?;
        let rep = plan::validate(src.as_mut(), &plan, trim, &ctx.cancel, &mut |p| {
            ctx.progress(p.read, 0, p.rejected, p.elapsed_ms)
        })
        .map_err(|e| e.to_string())?;
        let cancelled = rep.cancelled;
        Ok((serde_json::to_value(rep).unwrap_or_default(), cancelled))
    }))
}

#[tauri::command]
pub fn import_start(
    app: AppHandle,
    state: State<'_, AppState>,
    conn: String,
    path: String,
    read: ReadOptions,
    plan: MappingPlan,
    options: ImportOptions,
) -> CmdResult<String> {
    let c = current(&state, &conn)?;
    if plan.errors > 0 {
        return Err("the mapping still has errors; fix them first".into());
    }
    let opener = c.opener.clone();
    Ok(spawn_job(app, &state, "import", move |ctx| {
        let mut src = open_source(Path::new(&path), &read).map_err(|e| e.to_string())?;
        let db: Arc<dyn Database> = opener().map_err(|e| e.to_string())?;
        let rejects = PathBuf::from(format!("{path}.rejects.csv"));
        let rep = plan::import(
            db.as_ref(),
            src.as_mut(),
            &plan,
            &options,
            &rejects,
            &ctx.cancel,
            &mut |p| ctx.progress(p.read, p.inserted, p.rejected, p.elapsed_ms),
        )
        .map_err(|e| e.to_string())?;
        let cancelled = rep.cancelled;
        Ok((serde_json::to_value(rep).unwrap_or_default(), cancelled))
    }))
}
