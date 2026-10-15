//! Sandbox: a local SQLite copy of some Oracle tables (or the whole schema) to try things on.
//! Nothing here ever writes to Oracle. A clone reads with plain SELECTs on its own session, builds the
//! file next to the final name and renames it when complete, so a cancelled or failed clone leaves nothing.
//!
//! What is copied: columns, NOT NULL, primary and unique keys, normal indexes and the rows (all, or the
//! first N per table). What is not: foreign keys (relationships are recorded, not enforced, so partial data
//! loads), triggers, sequences, views, code, grants, LOB/binary content (those columns are kept and NULL).

mod dialect;
mod funcs;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rusqlite::{params_from_iter, Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::comments::quote_ident;
use crate::context::matches_pattern;
use crate::db::{collect, Database, Outcome};
use crate::{Error, Result};

pub use dialect::{to_sqlite, Translation};

fn sql_err(e: rusqlite::Error) -> Error {
    Error::Database(e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CloneSpec {
    /// Tables to copy. Ignored when `all` is set.
    pub tables: Vec<String>,
    pub all: bool,
    /// `None` copies every row; otherwise the first N rows of each table.
    pub rows_per_table: Option<u64>,
    /// Also copy the tables the chosen ones point to by foreign key, so joins still work.
    pub include_fk_parents: bool,
    /// Columns whose values are not copied (NULL instead): `COLUMN`, `TABLE.COLUMN`, `*` wildcards.
    pub mask: Vec<String>,
    pub with_indexes: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableBrief {
    pub name: String,
    pub rows: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColMeta {
    pub name: String,
    pub oracle_type: String,
    pub sqlite_type: String,
    /// Values were not copied because of the mask list.
    pub masked: bool,
    /// Values were not copied because the type is binary/LOB.
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableMeta {
    pub name: String,
    pub source_rows: Option<u64>,
    pub rows: u64,
    /// Fewer rows were copied than the source has (the row limit applied).
    pub truncated: bool,
    pub columns: Vec<ColMeta>,
    pub primary_key: Vec<String>,
    /// Tables this one points to by foreign key (recorded, not enforced).
    pub parents: Vec<String>,
    /// Added only because a chosen table points to it.
    pub added_as_parent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SandboxMeta {
    pub name: String,
    pub source: String,
    pub owner: String,
    pub taken_at: i64,
    pub spec: CloneSpec,
    pub tables: Vec<TableMeta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxInfo {
    pub meta: SandboxMeta,
    pub bytes: u64,
    /// The sandbox was changed after it was cloned; `reset` restores the clone.
    pub modified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloneProgress {
    pub table: String,
    pub tables_done: usize,
    pub tables_total: usize,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClonePlan {
    pub tables: Vec<PlanTable>,
    pub rows_to_copy: u64,
    /// Row counts come from optimizer statistics and may be stale or missing.
    pub estimated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanTable {
    pub name: String,
    pub source_rows: Option<u64>,
    pub rows_to_copy: Option<u64>,
    pub added_as_parent: bool,
}

pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 60
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(
            "a sandbox name is 1-60 letters, digits, - or _".into(),
        ))
    }
}

fn path_of(dir: &Path, name: &str) -> Result<PathBuf> {
    valid_name(name)?;
    Ok(dir.join(format!("{name}.sqlite")))
}

fn cell(r: &[Option<String>], i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

/// Tables of a schema with their statistics row counts, for the picker.
pub fn list_tables(db: &dyn Database, owner: &str) -> Result<Vec<TableBrief>> {
    let (_, rows) = collect(
        db,
        "SELECT table_name, num_rows FROM all_tables WHERE owner = :1 AND temporary = 'N' AND secondary = 'N' AND nested = 'NO' AND iot_name IS NULL ORDER BY table_name",
        &[owner.to_string()],
    )?;
    Ok(rows
        .iter()
        .map(|r| TableBrief {
            name: cell(r, 0),
            rows: cell(r, 1).parse().ok(),
        })
        .collect())
}

struct Col {
    table: String,
    name: String,
    data_type: String,
    precision: Option<i64>,
    scale: Option<i64>,
    not_null: bool,
}

struct Dict {
    tables: Vec<TableBrief>,
    cols: Vec<Col>,
    /// (table, kind P|U, constraint, columns in order)
    keys: Vec<(String, String, String, Vec<String>)>,
    /// (child, parent)
    fks: Vec<(String, String)>,
}

fn read_dict(db: &dyn Database, owner: &str) -> Result<Dict> {
    let o = [owner.to_string()];
    let tables = list_tables(db, owner)?;
    let (_, rows) = collect(
        db,
        "SELECT table_name, column_name, data_type, data_precision, data_scale, nullable FROM all_tab_columns WHERE owner = :1 ORDER BY table_name, column_id",
        &o,
    )?;
    let cols = rows
        .iter()
        .map(|r| Col {
            table: cell(r, 0),
            name: cell(r, 1),
            data_type: cell(r, 2),
            precision: cell(r, 3).parse().ok(),
            scale: cell(r, 4).parse().ok(),
            not_null: cell(r, 5) == "N",
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT c.table_name, c.constraint_type, c.constraint_name, cc.column_name FROM all_constraints c JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name WHERE c.owner = :1 AND c.constraint_type IN ('P','U') ORDER BY c.table_name, c.constraint_name, cc.position",
        &o,
    )?;
    let mut keys: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for r in &rows {
        let (t, k, n, c) = (cell(r, 0), cell(r, 1), cell(r, 2), cell(r, 3));
        match keys.last_mut() {
            Some(last) if last.0 == t && last.2 == n => last.3.push(c),
            _ => keys.push((t, k, n, vec![c])),
        }
    }
    let (_, rows) = collect(
        db,
        "SELECT c.table_name, p.table_name FROM all_constraints c JOIN all_constraints p ON p.owner = c.r_owner AND p.constraint_name = c.r_constraint_name WHERE c.owner = :1 AND c.constraint_type = 'R' AND c.r_owner = c.owner",
        &o,
    )?;
    let fks = rows.iter().map(|r| (cell(r, 0), cell(r, 1))).collect();
    Ok(Dict {
        tables,
        cols,
        keys,
        fks,
    })
}

/// The tables a spec selects, with the parents that were added for foreign keys.
fn select_tables(d: &Dict, spec: &CloneSpec) -> Result<Vec<(String, bool)>> {
    let known = |n: &str| d.tables.iter().any(|t| t.name == n);
    let mut chosen: Vec<(String, bool)> = if spec.all {
        d.tables.iter().map(|t| (t.name.clone(), false)).collect()
    } else {
        let mut v = Vec::new();
        for t in &spec.tables {
            let t = t.trim();
            if !known(t) {
                return Err(Error::Invalid(format!(
                    "table {t} was not found in this schema (or is not a plain table)"
                )));
            }
            if !v.iter().any(|(n, _)| n == t) {
                v.push((t.to_string(), false));
            }
        }
        v
    };
    if chosen.is_empty() {
        return Err(Error::Invalid(
            "choose at least one table, or the whole schema".into(),
        ));
    }
    if spec.include_fk_parents {
        let mut i = 0;
        while i < chosen.len() {
            let name = chosen[i].0.clone();
            for (c, p) in &d.fks {
                if *c == name && known(p) && !chosen.iter().any(|(n, _)| n == p) {
                    chosen.push((p.clone(), true));
                }
            }
            i += 1;
        }
    }
    Ok(chosen)
}

pub fn plan_clone(db: &dyn Database, owner: &str, spec: &CloneSpec) -> Result<ClonePlan> {
    let d = read_dict(db, owner)?;
    let chosen = select_tables(&d, spec)?;
    let mut total = 0u64;
    let tables: Vec<PlanTable> = chosen
        .iter()
        .map(|(n, parent)| {
            let src = d.tables.iter().find(|t| t.name == *n).and_then(|t| t.rows);
            let copy = match (src, spec.rows_per_table) {
                (Some(s), Some(l)) => Some(s.min(l)),
                (None, Some(l)) => Some(l),
                (s, None) => s,
            };
            total += copy.unwrap_or(0);
            PlanTable {
                name: n.clone(),
                source_rows: src,
                rows_to_copy: copy,
                added_as_parent: *parent,
            }
        })
        .collect();
    Ok(ClonePlan {
        tables,
        rows_to_copy: total,
        estimated: true,
    })
}

fn sqlite_type(
    data_type: &str,
    precision: Option<i64>,
    scale: Option<i64>,
) -> (&'static str, bool) {
    let t = data_type.to_uppercase();
    match t.as_str() {
        "NUMBER" if scale == Some(0) && precision.is_some_and(|p| p <= 18) => ("INTEGER", false),
        "NUMBER" => ("NUMERIC", false),
        "FLOAT" | "BINARY_FLOAT" | "BINARY_DOUBLE" => ("REAL", false),
        "BLOB" | "BFILE" | "LONG RAW" => ("BLOB", true),
        _ => ("TEXT", false),
    }
}

fn is_masked(mask: &[String], table: &str, col: &str) -> bool {
    mask.iter()
        .any(|p| matches_pattern(p, col) || matches_pattern(p, &format!("{table}.{col}")))
}

const CLONE_NLS: &str = "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM' NLS_NUMERIC_CHARACTERS = '.,'";

/// Copies the chosen tables into `<dir>/<name>.sqlite`. `db` must be a session this call may use freely.
#[allow(clippy::too_many_arguments)]
pub fn clone_schema(
    db: &dyn Database,
    owner: &str,
    source: &str,
    dir: &Path,
    name: &str,
    spec: &CloneSpec,
    now_ms: i64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(CloneProgress),
) -> Result<Option<SandboxMeta>> {
    let final_path = path_of(dir, name)?;
    if final_path.exists() {
        return Err(Error::Invalid(format!(
            "a sandbox named {name} already exists"
        )));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::Invalid(format!("cannot create {}: {e}", dir.display())))?;
    let part = dir.join(format!("{name}.sqlite.part"));
    let _ = std::fs::remove_file(&part);
    let result = clone_into(
        db, owner, source, &part, name, spec, now_ms, cancel, progress,
    );
    match result {
        Ok(Some(meta)) => {
            std::fs::rename(&part, &final_path)
                .map_err(|e| Error::Invalid(format!("cannot finish the sandbox file: {e}")))?;
            Ok(Some(meta))
        }
        Ok(None) => {
            let _ = std::fs::remove_file(&part);
            Ok(None)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn clone_into(
    db: &dyn Database,
    owner: &str,
    source: &str,
    part: &Path,
    name: &str,
    spec: &CloneSpec,
    now_ms: i64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(CloneProgress),
) -> Result<Option<SandboxMeta>> {
    let _ = db.run(CLONE_NLS, &[], 1);
    let _ = db.run("SET TRANSACTION READ ONLY", &[], 1); // best effort: nothing here may change Oracle
    let d = read_dict(db, owner)?;
    let chosen = select_tables(&d, spec)?;
    let lite = Connection::open(part).map_err(sql_err)?;
    lite.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")
        .map_err(sql_err)?;

    let mut metas: Vec<TableMeta> = Vec::new();
    let mut rows_total = 0u64;
    let started = Instant::now();
    let _ = started;
    for (idx, (tname, as_parent)) in chosen.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        progress(CloneProgress {
            table: tname.clone(),
            tables_done: idx,
            tables_total: chosen.len(),
            rows: rows_total,
        });
        let cols: Vec<&Col> = d.cols.iter().filter(|c| c.table == *tname).collect();
        if cols.is_empty() {
            continue;
        }
        let mut cmeta: Vec<ColMeta> = Vec::new();
        let mut defs: Vec<String> = Vec::new();
        let mut select: Vec<String> = Vec::new();
        for c in &cols {
            let (st, skipped) = sqlite_type(&c.data_type, c.precision, c.scale);
            let masked = is_masked(&spec.mask, tname, &c.name);
            let qc = quote_ident(&c.name)?;
            defs.push(format!(
                "{qc} {st}{}",
                if c.not_null && !masked && !skipped {
                    " NOT NULL"
                } else {
                    ""
                }
            ));
            select.push(if masked || skipped {
                format!("NULL AS {qc}")
            } else {
                qc
            });
            cmeta.push(ColMeta {
                name: c.name.clone(),
                oracle_type: c.data_type.clone(),
                sqlite_type: st.into(),
                masked,
                skipped,
            });
        }
        let masked_names: Vec<&str> = cmeta
            .iter()
            .filter(|c| c.masked || c.skipped)
            .map(|c| c.name.as_str())
            .collect();
        let mut pk: Vec<String> = Vec::new();
        for (t, kind, _, kcols) in d.keys.iter().filter(|k| k.0 == *tname) {
            let _ = t;
            if kcols.iter().any(|c| masked_names.contains(&c.as_str())) {
                continue; // a key over values that are not copied would be meaningless
            }
            let list = kcols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            if kind == "P" {
                pk = kcols.clone();
                defs.push(format!("PRIMARY KEY ({list})"));
            } else {
                defs.push(format!("UNIQUE ({list})"));
            }
        }
        let qt = quote_ident(tname)?;
        lite.execute_batch(&format!("CREATE TABLE {qt} ({});", defs.join(", ")))
            .map_err(sql_err)?;

        let mut sel = format!(
            "SELECT {} FROM {}.{qt}",
            select.join(", "),
            quote_ident(owner)?
        );
        if let Some(n) = spec.rows_per_table {
            sel.push_str(&format!(" WHERE ROWNUM <= {n}"));
        }
        let mut cursor = match db.run(&sel, &[], 5_000)? {
            Outcome::Rows(c) => c,
            _ => {
                return Err(Error::Database(format!(
                    "reading {tname} returned no result set"
                )))
            }
        };
        let blank: Vec<bool> = cmeta.iter().map(|c| c.masked || c.skipped).collect();
        let insert = format!(
            "INSERT INTO {qt} VALUES ({})",
            vec!["?"; cmeta.len()].join(", ")
        );
        let mut copied = 0u64;
        loop {
            let batch = cursor.next_batch(5_000)?;
            if batch.is_empty() {
                break;
            }
            let tx = lite.unchecked_transaction().map_err(sql_err)?;
            {
                let mut st = tx.prepare(&insert).map_err(sql_err)?;
                for row in &batch {
                    // The select list already sends NULL for these; blanking again here means a masked value can never be stored.
                    st.execute(params_from_iter(row.iter().zip(&blank).map(|(v, b)| {
                        if *b {
                            None
                        } else {
                            v.as_ref()
                        }
                    })))
                    .map_err(|e| Error::Database(format!("{tname}: {e}")))?;
                }
            }
            tx.commit().map_err(sql_err)?;
            copied += batch.len() as u64;
            rows_total += batch.len() as u64;
            progress(CloneProgress {
                table: tname.clone(),
                tables_done: idx,
                tables_total: chosen.len(),
                rows: rows_total,
            });
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
        }
        let source_rows = d
            .tables
            .iter()
            .find(|t| t.name == *tname)
            .and_then(|t| t.rows);
        let parents: Vec<String> = {
            let mut p: Vec<String> = d
                .fks
                .iter()
                .filter(|(c, _)| c == tname)
                .map(|(_, p)| p.clone())
                .collect();
            p.sort();
            p.dedup();
            p
        };
        metas.push(TableMeta {
            name: tname.clone(),
            source_rows,
            rows: copied,
            truncated: spec.rows_per_table.is_some_and(|l| copied >= l)
                && source_rows.is_none_or(|s| s > copied),
            columns: cmeta,
            primary_key: pk,
            parents,
            added_as_parent: *as_parent,
        });
    }

    if spec.with_indexes {
        let (_, rows) = collect(
            db,
            "SELECT i.table_name, i.index_name, i.uniqueness, ic.column_name FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name WHERE i.owner = :1 AND i.index_type = 'NORMAL' ORDER BY i.table_name, i.index_name, ic.column_position",
            &[owner.to_string()],
        )?;
        let mut idx: Vec<(String, String, bool, Vec<String>)> = Vec::new();
        for r in &rows {
            let (t, n, u, c) = (cell(r, 0), cell(r, 1), cell(r, 2) == "UNIQUE", cell(r, 3));
            match idx.last_mut() {
                Some(l) if l.0 == t && l.1 == n => l.3.push(c),
                _ => idx.push((t, n, u, vec![c])),
            }
        }
        for (t, n, uniq, cols) in idx {
            let Some(m) = metas.iter().find(|m| m.name == t) else {
                continue;
            };
            if cols.iter().any(|c| {
                m.columns
                    .iter()
                    .any(|x| x.name == *c && (x.masked || x.skipped))
            }) {
                continue;
            }
            let list = cols
                .iter()
                .map(|c| quote_ident(c))
                .collect::<Result<Vec<_>>>()?
                .join(", ");
            // A unique index over partial data is still unique; failure to build one is not fatal.
            let _ = lite.execute_batch(&format!(
                "CREATE {}INDEX {} ON {} ({list});",
                if uniq { "UNIQUE " } else { "" },
                quote_ident(&n)?,
                quote_ident(&t)?
            ));
        }
    }

    let meta = SandboxMeta {
        name: name.to_string(),
        source: source.to_string(),
        owner: owner.to_string(),
        taken_at: now_ms,
        spec: spec.clone(),
        tables: metas,
    };
    lite.execute_batch("CREATE TABLE _ora_sandbox (k TEXT PRIMARY KEY, v TEXT NOT NULL);")
        .map_err(sql_err)?;
    lite.execute(
        "INSERT INTO _ora_sandbox VALUES ('meta', ?)",
        [serde_json::to_string(&meta).map_err(|e| Error::Invalid(e.to_string()))?],
    )
    .map_err(sql_err)?;
    Ok(Some(meta))
}

fn read_meta(path: &Path) -> Result<SandboxMeta> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_err)?;
    let v: String = c
        .query_row("SELECT v FROM _ora_sandbox WHERE k = 'meta'", [], |r| {
            r.get(0)
        })
        .map_err(sql_err)?;
    serde_json::from_str(&v)
        .map_err(|e| Error::Invalid(format!("sandbox metadata is unreadable: {e}")))
}

pub fn list(dir: &Path) -> Vec<SandboxInfo> {
    let mut out: Vec<SandboxInfo> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("sqlite") {
            continue;
        }
        if let Ok(meta) = read_meta(&p) {
            let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
            let modified = base_path(&p).exists();
            out.push(SandboxInfo {
                meta,
                bytes,
                modified,
            });
        }
    }
    out.sort_by(|a, b| {
        b.meta
            .taken_at
            .cmp(&a.meta.taken_at)
            .then(a.meta.name.cmp(&b.meta.name))
    });
    out
}

fn base_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(".base");
    PathBuf::from(s)
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    let p = path_of(dir, name)?;
    if !p.exists() {
        return Err(Error::Invalid(format!("no sandbox named {name}")));
    }
    std::fs::remove_file(&p).map_err(|e| Error::Invalid(e.to_string()))?;
    let _ = std::fs::remove_file(base_path(&p));
    Ok(())
}

/// Puts the sandbox back to what the clone looked like. A no-op when nothing was changed.
pub fn reset(dir: &Path, name: &str) -> Result<bool> {
    let p = path_of(dir, name)?;
    let base = base_path(&p);
    if !base.exists() {
        return Ok(false);
    }
    let tmp = dir.join(format!("{name}.sqlite.restore"));
    std::fs::copy(&base, &tmp).map_err(|e| Error::Invalid(format!("cannot restore: {e}")))?;
    std::fs::rename(&tmp, &p).map_err(|e| Error::Invalid(format!("cannot restore: {e}")))?;
    std::fs::remove_file(&base).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(true)
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxRun {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// Set for INSERT/UPDATE/DELETE.
    pub affected: Option<usize>,
    pub truncated: bool,
    pub elapsed_ms: u64,
    /// What was sent to SQLite when it differs from what was typed.
    pub translated: Option<String>,
    pub changes: Vec<String>,
    /// The statement changed the sandbox.
    pub wrote: bool,
}

const MAX_RESULT_ROWS: usize = 20_000;

fn show(v: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef::*;
    match v {
        Null => None,
        Integer(i) => Some(i.to_string()),
        Real(f) => Some(if f.fract() == 0.0 && f.abs() < 1e15 {
            format!("{}", f as i64)
        } else {
            f.to_string()
        }),
        Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        Blob(b) => Some(format!("(binary, {} bytes)", b.len())),
    }
}

/// Runs one statement in the sandbox. The first statement that changes anything keeps a copy of the
/// clone first, so `reset` can always go back.
pub fn run(dir: &Path, name: &str, sql: &str) -> Result<SandboxRun> {
    let p = path_of(dir, name)?;
    if !p.exists() {
        return Err(Error::Invalid(format!("no sandbox named {name}")));
    }
    let tr = to_sqlite(sql);
    if let Some(why) = tr.refused {
        return Err(Error::Invalid(why));
    }
    let first = tr
        .sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_uppercase();
    if tr.sql.is_empty() {
        return Err(Error::Invalid("empty statement".into()));
    }
    if matches!(
        first.as_str(),
        "ATTACH" | "DETACH" | "PRAGMA" | "VACUUM" | "REINDEX"
    ) || tr.sql.to_lowercase().contains("_ora_sandbox")
    {
        return Err(Error::Blocked(
            "this statement is not allowed in the sandbox".into(),
        ));
    }
    let t0 = Instant::now();
    let conn = Connection::open(&p).map_err(sql_err)?;
    funcs::register(&conn).map_err(sql_err)?;
    let mut stmt = conn
        .prepare(&tr.sql)
        .map_err(|e| Error::Database(hint(&e.to_string())))?;
    let writes = !stmt.readonly();
    let ncols = stmt.column_count();
    let base = base_path(&p);
    if writes && !base.exists() {
        drop(stmt);
        drop(conn);
        std::fs::copy(&p, &base)
            .map_err(|e| Error::Invalid(format!("cannot keep a copy of the clone: {e}")))?;
        return run(dir, name, sql);
    }
    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let (mut rows, mut truncated, mut affected) = (Vec::new(), false, None);
    if ncols > 0 {
        let mut q = stmt
            .query([])
            .map_err(|e| Error::Database(hint(&e.to_string())))?;
        while let Some(r) = q
            .next()
            .map_err(|e| Error::Database(hint(&e.to_string())))?
        {
            if rows.len() >= MAX_RESULT_ROWS {
                truncated = true;
                break;
            }
            rows.push(
                (0..ncols)
                    .map(|i| r.get_ref(i).ok().and_then(show))
                    .collect(),
            );
        }
    } else {
        affected = Some(
            stmt.execute([])
                .map_err(|e| Error::Database(hint(&e.to_string())))?,
        );
    }
    Ok(SandboxRun {
        columns,
        rows,
        affected,
        truncated,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        translated: (tr.sql != sql.trim().trim_end_matches(';').trim_end())
            .then_some(tr.sql.clone()),
        changes: tr.changes,
        wrote: writes,
    })
}

/// Makes SQLite's wording useful to someone who knows Oracle.
fn hint(msg: &str) -> String {
    if let Some(f) = msg.strip_prefix("no such function: ") {
        return format!("{msg}. The Oracle function {f} is not available in the sandbox; the sandbox covers NVL, NVL2, DECODE, TO_CHAR, TO_DATE, TO_NUMBER, TRUNC, CEIL, FLOOR, POWER, SQRT, MOD, LPAD, RPAD, INITCAP, ADD_MONTHS and LAST_DAY.");
    }
    if msg.starts_with("no such table") {
        return format!("{msg} (only the cloned tables exist here; clone it, or tick \"include tables they point to\")");
    }
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn oracle() -> FakeDb {
        FakeDb::new()
            .with_table(
                "from all_tables",
                FakeTable::new(
                    &["TABLE_NAME", "NUM_ROWS"],
                    vec![
                        vec![Some("DEPARTMENTS"), Some("2")],
                        vec![Some("EMPLOYEES"), Some("3")],
                    ],
                ),
            )
            .with_table(
                "from all_tab_columns",
                FakeTable::new(
                    &["T", "C", "TYPE", "P", "S", "N"],
                    vec![
                        vec![
                            Some("DEPARTMENTS"),
                            Some("ID"),
                            Some("NUMBER"),
                            Some("4"),
                            Some("0"),
                            Some("N"),
                        ],
                        vec![
                            Some("DEPARTMENTS"),
                            Some("NAME"),
                            Some("VARCHAR2"),
                            None,
                            None,
                            Some("Y"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("ID"),
                            Some("NUMBER"),
                            Some("10"),
                            Some("0"),
                            Some("N"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("NAME"),
                            Some("VARCHAR2"),
                            None,
                            None,
                            Some("N"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("SALARY"),
                            Some("NUMBER"),
                            Some("10"),
                            Some("2"),
                            Some("Y"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("HIRED"),
                            Some("DATE"),
                            None,
                            None,
                            Some("Y"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("PHOTO"),
                            Some("BLOB"),
                            None,
                            None,
                            Some("Y"),
                        ],
                        vec![
                            Some("EMPLOYEES"),
                            Some("DEPT_ID"),
                            Some("NUMBER"),
                            Some("4"),
                            Some("0"),
                            Some("Y"),
                        ],
                    ],
                ),
            )
            .with_table(
                "join all_cons_columns",
                FakeTable::new(
                    &["T", "K", "N", "C"],
                    vec![
                        vec![Some("DEPARTMENTS"), Some("P"), Some("DEPT_PK"), Some("ID")],
                        vec![Some("EMPLOYEES"), Some("P"), Some("EMP_PK"), Some("ID")],
                    ],
                ),
            )
            .with_table(
                "join all_constraints p",
                FakeTable::new(
                    &["C", "P"],
                    vec![vec![Some("EMPLOYEES"), Some("DEPARTMENTS")]],
                ),
            )
            .with_table(
                "from all_indexes",
                FakeTable::new(
                    &["T", "I", "U", "C"],
                    vec![vec![
                        Some("EMPLOYEES"),
                        Some("EMP_DEPT_IX"),
                        Some("NONUNIQUE"),
                        Some("DEPT_ID"),
                    ]],
                ),
            )
            .with_table(
                "\"departments\"",
                FakeTable::new(
                    &["ID", "NAME"],
                    vec![vec![Some("10"), Some("Sales")], vec![Some("20"), None]],
                ),
            )
            .with_table(
                "\"employees\"",
                FakeTable::new(
                    &["ID", "NAME", "SALARY", "HIRED", "PHOTO", "DEPT_ID"],
                    vec![
                        vec![
                            Some("1"),
                            Some("KING"),
                            Some("5000.50"),
                            Some("2020-01-15 00:00:00"),
                            None,
                            Some("10"),
                        ],
                        vec![
                            Some("2"),
                            Some("BLAKE"),
                            Some("3000"),
                            Some("2021-03-04 09:30:00"),
                            None,
                            Some("20"),
                        ],
                        vec![Some("3"), Some("CLARK"), None, None, None, Some("10")],
                    ],
                ),
            )
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ora_ide_sb_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn all() -> CloneSpec {
        CloneSpec {
            all: true,
            with_indexes: true,
            ..Default::default()
        }
    }

    fn make(tag: &str, spec: &CloneSpec) -> (PathBuf, SandboxMeta) {
        let d = dir(tag);
        let m = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "demo",
            spec,
            1_000,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
        (d, m)
    }

    #[test]
    fn the_whole_schema_is_cloned_with_types_keys_indexes_and_rows() {
        let (d, m) = make("all", &all());
        assert_eq!(
            m.tables
                .iter()
                .map(|t| (t.name.as_str(), t.rows))
                .collect::<Vec<_>>(),
            vec![("DEPARTMENTS", 2), ("EMPLOYEES", 3)]
        );
        let e = &m.tables[1];
        assert_eq!(e.primary_key, vec!["ID"]);
        assert_eq!(e.parents, vec!["DEPARTMENTS"]);
        let ty: Vec<_> = e
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.sqlite_type.as_str(), c.skipped))
            .collect();
        assert_eq!(
            ty,
            vec![
                ("ID", "INTEGER", false),
                ("NAME", "TEXT", false),
                ("SALARY", "NUMERIC", false),
                ("HIRED", "TEXT", false),
                ("PHOTO", "BLOB", true),
                ("DEPT_ID", "INTEGER", false)
            ]
        );
        let r = run(&d, "demo", "select e.name, d.name as dept from employees e join departments d on d.id = e.dept_id order by e.id").unwrap();
        assert_eq!(r.rows.len(), 3);
        assert_eq!(
            r.rows[0],
            vec![Some("KING".to_string()), Some("Sales".to_string())]
        );
        assert_eq!(r.rows[1], vec![Some("BLAKE".to_string()), None]);
        let r = run(
            &d,
            "demo",
            "select name from sqlite_master where type = 'index' and name = 'EMP_DEPT_IX'",
        )
        .unwrap();
        assert_eq!(r.rows.len(), 1, "indexes are cloned");
        let info = list(&d);
        assert_eq!(info.len(), 1);
        assert!(info[0].bytes > 0 && !info[0].modified && info[0].meta.owner == "HR");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn numbers_keep_their_values_and_binary_columns_are_null() {
        let (d, _) = make("types", &all());
        let r = run(
            &d,
            "demo",
            "select id, salary, hired, photo from employees order by id",
        )
        .unwrap();
        assert_eq!(
            r.rows[0],
            vec![
                Some("1".into()),
                Some("5000.5".into()),
                Some("2020-01-15 00:00:00".into()),
                None
            ]
        );
        assert_eq!(r.rows[2][1], None, "NULL stays NULL");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn masked_columns_are_not_copied_and_do_not_form_keys() {
        let spec = CloneSpec {
            all: true,
            mask: vec!["SALARY".into(), "EMPLOYEES.NAME".into()],
            ..Default::default()
        };
        let (d, m) = make("mask", &spec);
        let e = &m.tables[1];
        assert!(e
            .columns
            .iter()
            .filter(|c| c.masked)
            .map(|c| c.name.as_str())
            .eq(["NAME", "SALARY"]));
        let r = run(
            &d,
            "demo",
            "select count(*), count(name), count(salary) from employees",
        )
        .unwrap();
        assert_eq!(
            r.rows[0],
            vec![Some("3".into()), Some("0".into()), Some("0".into())]
        );
        // NOT NULL is dropped for a masked column, otherwise the NULLs could not be stored (the insert above would fail).
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_chosen_table_can_bring_the_tables_it_points_to() {
        let d = dir("parents");
        let spec = CloneSpec {
            tables: vec!["EMPLOYEES".into()],
            include_fk_parents: true,
            ..Default::default()
        };
        let plan = plan_clone(&oracle(), "HR", &spec).unwrap();
        assert_eq!(
            plan.tables
                .iter()
                .map(|t| (t.name.as_str(), t.added_as_parent))
                .collect::<Vec<_>>(),
            vec![("EMPLOYEES", false), ("DEPARTMENTS", true)]
        );
        assert_eq!(plan.rows_to_copy, 5);
        let m = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "demo",
            &spec,
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
        assert!(m
            .tables
            .iter()
            .any(|t| t.name == "DEPARTMENTS" && t.added_as_parent));
        let only = CloneSpec {
            tables: vec!["EMPLOYEES".into()],
            ..Default::default()
        };
        let m2 = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "only",
            &only,
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(m2.tables.len(), 1);
        let e = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "bad",
            &CloneSpec {
                tables: vec!["NOPE".into()],
                ..Default::default()
            },
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.to_string().contains("NOPE"), "{e}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_row_limit_is_applied_in_the_source_query() {
        let db = oracle();
        let d = dir("limit");
        let spec = CloneSpec {
            tables: vec!["DEPARTMENTS".into()],
            rows_per_table: Some(1),
            ..Default::default()
        };
        clone_schema(
            &db,
            "HR",
            "dev",
            &d,
            "demo",
            &spec,
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let log = db.log.lock().unwrap();
        assert!(log
            .calls
            .iter()
            .any(|(q, _)| q.contains("WHERE ROWNUM <= 1") && q.contains("\"DEPARTMENTS\"")));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn cancel_leaves_no_file_and_a_name_cannot_be_reused() {
        let d = dir("cancel");
        let stop = AtomicBool::new(true);
        let r = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "demo",
            &all(),
            1,
            &stop,
            &mut |_| {},
        )
        .unwrap();
        assert!(r.is_none());
        assert!(
            std::fs::read_dir(&d)
                .map(|mut x| x.next().is_none())
                .unwrap_or(true),
            "no .part left behind"
        );
        clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "demo",
            &all(),
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let e = clone_schema(
            &oracle(),
            "HR",
            "dev",
            &d,
            "demo",
            &all(),
            1,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.to_string().contains("already exists"));
        for bad in ["", "a/b", "..", "x y", "-x", &"n".repeat(61)] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
        assert!(valid_name("hr_dev-2").is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn changes_keep_a_copy_and_reset_goes_back() {
        let (d, _) = make("reset", &all());
        assert!(!reset(&d, "demo").unwrap(), "nothing to undo yet");
        let w = run(&d, "demo", "update employees set salary = 1 where id = 1").unwrap();
        assert_eq!((w.affected, w.wrote), (Some(1), true));
        run(&d, "demo", "delete from departments").unwrap();
        assert!(list(&d)[0].modified);
        assert_eq!(
            run(&d, "demo", "select count(*) from departments")
                .unwrap()
                .rows[0][0]
                .as_deref(),
            Some("0")
        );
        assert!(reset(&d, "demo").unwrap());
        assert_eq!(
            run(&d, "demo", "select count(*) from departments")
                .unwrap()
                .rows[0][0]
                .as_deref(),
            Some("2")
        );
        assert_eq!(
            run(&d, "demo", "select salary from employees where id = 1")
                .unwrap()
                .rows[0][0]
                .as_deref(),
            Some("5000.5")
        );
        assert!(!list(&d)[0].modified);
        delete(&d, "demo").unwrap();
        assert!(list(&d).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn oracle_sql_runs_after_translation_and_unsafe_or_impossible_statements_do_not() {
        let (d, _) = make("dialect", &all());
        let r = run(&d, "demo", "select nvl(salary, 0) as s, decode(dept_id, 10, 'Sales', 'Other') as d from employees order by id fetch first 2 rows only").unwrap();
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0], vec![Some("5000.5".into()), Some("Sales".into())]);
        assert!(
            r.translated
                .as_deref()
                .is_some_and(|t| t.ends_with("LIMIT 2")),
            "{:?}",
            r.translated
        );
        assert_eq!(
            run(&d, "demo", "select 1 from dual").unwrap().rows[0][0].as_deref(),
            Some("1")
        );
        assert_eq!(
            run(
                &d,
                "demo",
                "select to_char(to_date('2021-03-04','YYYY-MM-DD'), 'DD-MON-YYYY') from dual"
            )
            .unwrap()
            .rows[0][0]
                .as_deref(),
            Some("04-MAR-2021")
        );
        for (sql, why) in [
            ("begin null; end;", "PL/SQL"),
            ("select * from employees where rownum < 2", "ROWNUM"),
            ("attach database 'x' as y", "not allowed"),
            ("pragma writable_schema = 1", "not allowed"),
            ("select * from _ora_sandbox", "not allowed"),
            ("vacuum", "not allowed"),
        ] {
            let e = run(&d, "demo", sql).unwrap_err().to_string();
            assert!(e.contains(why), "{sql}: {e}");
        }
        let e = run(&d, "demo", "select regexp_substr(name, 'a') from employees")
            .unwrap_err()
            .to_string();
        assert!(e.contains("not available in the sandbox"), "{e}");
        let e = run(&d, "demo", "select * from nope")
            .unwrap_err()
            .to_string();
        assert!(e.contains("only the cloned tables"), "{e}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_table_picker_lists_tables_with_statistics_counts() {
        let t = list_tables(&oracle(), "HR").unwrap();
        assert_eq!(
            t.iter()
                .map(|x| (x.name.as_str(), x.rows))
                .collect::<Vec<_>>(),
            vec![("DEPARTMENTS", Some(2)), ("EMPLOYEES", Some(3))]
        );
    }
}
