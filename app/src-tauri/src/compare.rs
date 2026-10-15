//! Comparison commands: text diff, key-based data compare (files, queries, tables) and schema compare.

use std::path::PathBuf;
use std::sync::Arc;

use ora_core::datadiff::{self, Cell, CursorStream, DataDiffOptions, FileStream, RowStream};
use ora_core::db::{Database, Outcome};
use ora_core::guard::{analyze, StatementKind};
use ora_core::import::source::ReadOptions;
use ora_core::profile::Environment;
use ora_core::schemadiff::{self, SchemaDiff};
use ora_core::textdiff::{self, TextDiffResult, TextOptions};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::{current, AppState};
use crate::jobs::{spawn_job, EXPORT_NLS};

type CmdResult<T> = Result<T, String>;

// ------------------------------------------------------------------------------------ text

#[tauri::command]
pub async fn text_diff(
    a: String,
    b: String,
    options: TextOptions,
    context: Option<usize>,
) -> CmdResult<TextDiffResult> {
    tauri::async_runtime::spawn_blocking(move || textdiff::diff_text(&a, &b, &options, context))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn text_patch(
    a: String,
    b: String,
    options: TextOptions,
    a_name: String,
    b_name: String,
    context: usize,
) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        textdiff::unified_patch(&a, &b, &options, &a_name, &b_name, context)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn text_read_file(path: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || {
        textdiff::read_text_file(std::path::Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Saves text (a patch, a script) where the person chose; written to a temporary file first.
#[tauri::command]
pub async fn text_write_file(path: String, content: String) -> CmdResult<()> {
    let p = PathBuf::from(&path);
    if !p.is_absolute() || !p.parent().is_some_and(|d| d.is_dir()) {
        return Err("choose a file in an existing folder".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let tmp = p.with_extension("part");
        std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------------------------------ data

#[derive(Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataSide {
    File {
        path: String,
        read: ReadOptions,
    },
    Query {
        conn: String,
        sql: String,
    },
    Table {
        conn: String,
        owner: String,
        table: String,
    },
}

type Opener = Arc<dyn Fn() -> ora_core::Result<Arc<dyn Database>> + Send + Sync>;

enum Resolved {
    File { path: PathBuf, read: ReadOptions },
    Db { opener: Opener, sql: String },
}

fn resolve(state: &State<'_, AppState>, side: DataSide) -> CmdResult<Resolved> {
    let (conn, sql) = match side {
        DataSide::File { path, read } => {
            let p = PathBuf::from(path);
            if !p.is_file() {
                return Err(format!("{} is not a file", p.display()));
            }
            return Ok(Resolved::File { path: p, read });
        }
        DataSide::Query { conn, sql } => (conn, sql),
        DataSide::Table { conn, owner, table } => {
            let sql = format!(
                "SELECT * FROM {}.{}",
                ora_core::comments::quote_ident(&owner).map_err(|e| e.to_string())?,
                ora_core::comments::quote_ident(&table).map_err(|e| e.to_string())?
            );
            (conn, sql)
        }
    };
    let c = current(state, &conn)?;
    let a = analyze(&sql, Environment::Dev);
    if a.kind != StatementKind::Select || a.blocked.is_some() || !a.read_only {
        return Err(a
            .blocked
            .unwrap_or_else(|| "only a single SELECT statement can be compared".into()));
    }
    Ok(Resolved::Db {
        opener: c.opener.clone(),
        sql: a.sql,
    })
}

fn open_stream(r: Resolved) -> CmdResult<Box<dyn RowStream>> {
    match r {
        Resolved::File { path, read } => Ok(Box::new(
            FileStream::open(&path, &read).map_err(|e| e.to_string())?,
        )),
        Resolved::Db { opener, sql } => {
            // A second session with fixed number/date formats, read-only where the database allows it.
            let db = opener().map_err(|e| e.to_string())?;
            db.run(EXPORT_NLS, &[], 1).map_err(|e| e.to_string())?;
            let _ = db.run("SET TRANSACTION READ ONLY", &[], 1);
            match db.run(&sql, &[], 10_000).map_err(|e| e.to_string())? {
                Outcome::Rows(c) => Ok(Box::new(KeepAlive {
                    inner: CursorStream::new(c),
                    _db: db,
                })),
                _ => Err("the statement returned no rows".into()),
            }
        }
    }
}

/// Keeps the session open as long as its cursor is read.
struct KeepAlive {
    inner: CursorStream,
    _db: Arc<dyn Database>,
}

impl RowStream for KeepAlive {
    fn columns(&self) -> &[String] {
        self.inner.columns()
    }
    fn next_row(&mut self) -> ora_core::Result<Option<Vec<Cell>>> {
        self.inner.next_row()
    }
}

#[derive(Serialize)]
pub struct SidePreview {
    columns: Vec<String>,
    sample: Vec<Vec<Cell>>,
    suggested_keys: Vec<Vec<String>>,
}

/// Column names, the first rows, and suggested key columns for one side.
#[tauri::command]
pub async fn data_side_preview(
    state: State<'_, AppState>,
    side: DataSide,
) -> CmdResult<SidePreview> {
    let r = resolve(&state, side)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = open_stream(r)?;
        let columns = s.columns().to_vec();
        let mut sample = Vec::new();
        while sample.len() < 2000 {
            match s.next_row().map_err(|e| e.to_string())? {
                Some(r) => sample.push(r),
                None => break,
            }
        }
        let suggested_keys = datadiff::suggest_keys(&columns, &sample);
        sample.truncate(8);
        Ok(SidePreview {
            columns,
            sample,
            suggested_keys,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Compares two sides by key as a background job (`job-progress` / `job-done`, kind `compare`).
#[tauri::command]
pub fn data_diff_start(
    app: AppHandle,
    state: State<'_, AppState>,
    a: DataSide,
    b: DataSide,
    options: DataDiffOptions,
    out_path: Option<String>,
) -> CmdResult<String> {
    let (ra, rb) = (resolve(&state, a)?, resolve(&state, b)?);
    let out = match out_path.filter(|p| !p.trim().is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            if !p.is_absolute() || !p.parent().is_some_and(|d| d.is_dir()) {
                return Err(
                    "the differences file needs an absolute path in an existing folder".into(),
                );
            }
            Some(p)
        }
        None => None,
    };
    Ok(spawn_job(app, &state, "compare", move |ctx| {
        let mut sa = open_stream(ra)?;
        let mut sb = open_stream(rb)?;
        let started = std::time::Instant::now();
        let report = datadiff::compare(
            sa.as_mut(),
            sb.as_mut(),
            &options,
            out.as_deref(),
            &ctx.cancel,
            &mut |ra, rb| ctx.progress(ra + rb, 0, 0, started.elapsed().as_millis() as u64),
        )
        .map_err(|e| e.to_string())?;
        let cancelled = report.cancelled;
        Ok((serde_json::to_value(report).unwrap_or_default(), cancelled))
    }))
}

// ---------------------------------------------------------------------------------- schema

/// Reads both schemas (each on its own session, in parallel) and compares them.
#[tauri::command]
pub async fn schema_compare(
    state: State<'_, AppState>,
    conn_a: String,
    owner_a: String,
    conn_b: String,
    owner_b: String,
    include_code: bool,
) -> CmdResult<SchemaDiff> {
    let (ca, cb) = (current(&state, &conn_a)?, current(&state, &conn_b)?);
    tauri::async_runtime::spawn_blocking(move || {
        let read = |c: crate::commands::Connected, owner: String| {
            let db = (c.opener)().unwrap_or_else(|_| c.db.clone());
            let kb = ora_core::knowledge::collect_schema(db.as_ref(), &owner, 0)?;
            let code = if include_code {
                Some(schemadiff::load_code_hashes(db.as_ref(), &owner)?)
            } else {
                None
            };
            Ok::<_, ora_core::Error>((kb, code))
        };
        let (ra, rb) = std::thread::scope(|s| {
            let ha = s.spawn(|| read(ca, owner_a));
            let hb = s.spawn(|| read(cb, owner_b));
            (ha.join(), hb.join())
        });
        let ((ka, code_a), (kb, code_b)) = (
            ra.map_err(|_| "reading side A failed".to_string())?
                .map_err(|e| format!("side A: {e}"))?,
            rb.map_err(|_| "reading side B failed".to_string())?
                .map_err(|e| format!("side B: {e}"))?,
        );
        Ok(schemadiff::compare_schemas(
            &ka,
            &kb,
            code_a.as_ref(),
            code_b.as_ref(),
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}
