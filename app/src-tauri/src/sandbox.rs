//! Sandbox commands: clone Oracle tables into a local SQLite file and run SQL there.
//! Nothing here writes to Oracle; the clone reads on its own session.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use ora_core::sandbox::{self, ClonePlan, CloneSpec, SandboxInfo, SandboxRun, TableBrief};
use tauri::{AppHandle, Manager, State};

use crate::commands::{current, AppState};
use crate::jobs::spawn_job;

type CmdResult<T> = Result<T, String>;

fn dir(app: &AppHandle) -> CmdResult<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("sandboxes"))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> ora_core::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sandbox_tables(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
) -> CmdResult<Vec<TableBrief>> {
    let c = current(&state, &conn)?;
    let db = c.db.clone();
    blocking(move || sandbox::list_tables(db.as_ref(), &owner)).await
}

#[tauri::command]
pub async fn sandbox_plan(
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    spec: CloneSpec,
) -> CmdResult<ClonePlan> {
    let c = current(&state, &conn)?;
    let db = c.db.clone();
    blocking(move || sandbox::plan_clone(db.as_ref(), &owner, &spec)).await
}

/// Copies the chosen tables on a background session; progress and the result arrive as job events.
#[tauri::command]
pub fn sandbox_clone_start(
    app: AppHandle,
    state: State<'_, AppState>,
    conn: String,
    owner: String,
    name: String,
    spec: CloneSpec,
) -> CmdResult<String> {
    sandbox::valid_name(&name).map_err(|e| e.to_string())?;
    let c = current(&state, &conn)?;
    let opener = c.opener.clone();
    let source = c.name.clone();
    let out = dir(&app)?;
    Ok(spawn_job(app, &state, "sandbox", move |ctx| {
        let db = opener().map_err(|e| e.to_string())?;
        let t0 = std::time::Instant::now();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let meta = sandbox::clone_schema(
            db.as_ref(),
            &owner,
            &source,
            &out,
            &name,
            &spec,
            now,
            &ctx.cancel,
            &mut |p| ctx.progress(p.rows, 0, 0, t0.elapsed().as_millis() as u64),
        )
        .map_err(|e| e.to_string())?;
        let cancelled = meta.is_none() || ctx.cancel.load(Ordering::Relaxed);
        Ok((serde_json::to_value(meta).unwrap_or_default(), cancelled))
    }))
}

#[tauri::command]
pub fn sandbox_list(app: AppHandle) -> CmdResult<Vec<SandboxInfo>> {
    Ok(sandbox::list(&dir(&app)?))
}

#[tauri::command]
pub async fn sandbox_delete(app: AppHandle, name: String) -> CmdResult<()> {
    let d = dir(&app)?;
    blocking(move || sandbox::delete(&d, &name)).await
}

#[tauri::command]
pub async fn sandbox_reset(app: AppHandle, name: String) -> CmdResult<bool> {
    let d = dir(&app)?;
    blocking(move || sandbox::reset(&d, &name)).await
}

#[tauri::command]
pub async fn sandbox_run(app: AppHandle, name: String, sql: String) -> CmdResult<SandboxRun> {
    let d = dir(&app)?;
    blocking(move || sandbox::run(&d, &name, &sql)).await
}
