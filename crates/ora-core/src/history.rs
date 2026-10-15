//! Local, persistent history of every statement executed and of worksheets.
//!
//! One SQLite file (WAL, FTS5). Query text is stored once per distinct
//! canonical form; every execution is a small row pointing at it, so a year of
//! history stays compact and searches stay fast. Passwords are masked before
//! anything is written to history; worksheet text is the user's own script and
//! is stored as typed.

use std::path::Path;

use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::guard::{analyze, StatementKind};
use crate::profile::Environment;
use crate::search::fts_query;
use crate::sqltext::{
    auto_title, fingerprint, redact_secrets, split_statements, sql_hash, table_names,
};
use crate::{Error, Result};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sql_texts(
  id INTEGER PRIMARY KEY,
  hash TEXT NOT NULL UNIQUE,
  text TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  kind TEXT NOT NULL,
  tables TEXT NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS sql_fts USING fts5(text, tables, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS executions(
  id INTEGER PRIMARY KEY,
  hash TEXT NOT NULL,
  conn_key TEXT NOT NULL,
  conn_name TEXT NOT NULL,
  owner TEXT NOT NULL,
  worksheet TEXT,
  started_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL,
  rows INTEGER,
  status TEXT NOT NULL,
  error TEXT
);
CREATE INDEX IF NOT EXISTS ex_hash ON executions(hash, started_at);
CREATE INDEX IF NOT EXISTS ex_time ON executions(started_at);
CREATE INDEX IF NOT EXISTS ex_ws ON executions(worksheet, started_at);
CREATE INDEX IF NOT EXISTS ex_conn ON executions(conn_key, started_at);
CREATE TABLE IF NOT EXISTS worksheets(
  id INTEGER PRIMARY KEY,
  uuid TEXT NOT NULL UNIQUE,
  conn_key TEXT NOT NULL,
  conn_name TEXT NOT NULL,
  owner TEXT NOT NULL,
  title TEXT,
  auto_title TEXT NOT NULL,
  sql TEXT NOT NULL,
  outline TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  closed_at INTEGER,
  pinned INTEGER NOT NULL DEFAULT 0,
  position INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS ws_state ON worksheets(closed_at, updated_at);
CREATE VIRTUAL TABLE IF NOT EXISTS ws_fts USING fts5(title, sql, tokenize = 'unicode61 remove_diacritics 2');
";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExecStatus {
    Ok,
    Error,
    Cancelled,
}

impl ExecStatus {
    fn as_str(self) -> &'static str {
        match self {
            ExecStatus::Ok => "ok",
            ExecStatus::Error => "error",
            ExecStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecRecord {
    pub sql: String,
    pub conn_key: String,
    pub conn_name: String,
    pub owner: String,
    pub worksheet: Option<String>,
    /// Unix milliseconds, UTC.
    pub started_at: i64,
    /// Time until the statement returned its first rows / finished.
    pub duration_ms: i64,
    pub rows: Option<i64>,
    pub status: ExecStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HistoryQuery {
    pub text: String,
    pub conn_key: Option<String>,
    /// Only queries that have succeeded at least once (what you would want to re-run).
    pub only_ok: bool,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub kinds: Vec<String>,
    pub pinned_only: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HistoryHit {
    pub hash: String,
    pub sql: String,
    pub kind: String,
    pub tables: Vec<String>,
    pub runs: i64,
    pub ok_runs: i64,
    pub first_run: i64,
    pub last_run: i64,
    pub avg_ms: Option<f64>,
    pub last_status: String,
    pub last_conn_name: String,
    pub last_conn_key: String,
    pub last_owner: String,
    pub pinned: bool,
    /// Matching text with `[`..`]` around matched words (text search only).
    pub snippet: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutlineItem {
    pub kind: String,
    pub tables: String,
    pub preview: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorksheetSave {
    pub uuid: String,
    pub conn_key: String,
    pub conn_name: String,
    pub owner: String,
    /// A title typed by the user; `None` keeps the automatic one.
    pub title: Option<String>,
    pub sql: String,
    pub position: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Worksheet {
    pub uuid: String,
    pub conn_key: String,
    pub conn_name: String,
    pub owner: String,
    pub title: String,
    pub custom_title: bool,
    pub sql: String,
    pub outline: Vec<OutlineItem>,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    pub pinned: bool,
    pub position: i64,
    pub runs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WsState {
    Open,
    Closed,
    #[default]
    All,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorksheetFilter {
    pub state: WsState,
    pub text: String,
    pub conn_key: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Step {
    pub at: i64,
    pub hash: String,
    pub sql: String,
    pub kind: String,
    pub status: String,
    pub duration_ms: i64,
    pub rows: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HistoryStats {
    pub executions: i64,
    pub distinct_queries: i64,
    pub worksheets: i64,
    pub first_at: Option<i64>,
    pub size_bytes: i64,
}

pub struct HistoryStore {
    conn: Connection,
}

fn kind_name(k: StatementKind) -> &'static str {
    match k {
        StatementKind::Select => "select",
        StatementKind::Insert => "insert",
        StatementKind::Update => "update",
        StatementKind::Delete => "delete",
        StatementKind::Merge => "merge",
        StatementKind::Ddl => "ddl",
        StatementKind::PlSql => "plsql",
        StatementKind::Transaction => "transaction",
        StatementKind::Other => "other",
        StatementKind::Empty => "empty",
    }
}

fn preview(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > 140 {
        format!("{}…", one.chars().take(140).collect::<String>())
    } else {
        one
    }
}

fn collapse_upper(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

impl HistoryStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(Error::Invalid(format!(
                "history file is from a newer version (schema {v})"
            )));
        }
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    // ------------------------------------------------------------ executions

    /// Record one execution. The text is redacted, de-duplicated by canonical form,
    /// and the stored copy keeps the formatting of the most recent run.
    pub fn record_execution(&mut self, r: &ExecRecord) -> Result<i64> {
        let text = redact_secrets(r.sql.trim().trim_end_matches(';').trim());
        if text.is_empty() {
            return Err(Error::Invalid("empty statement".into()));
        }
        let hash = sql_hash(&text);
        let kind = kind_name(analyze(&text, Environment::Dev).kind);
        let tables = table_names(&text);
        let tx = self.conn.transaction()?;
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM sql_texts WHERE hash = ?1", [&hash], |x| {
                x.get(0)
            })
            .optional()?;
        match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE sql_texts SET text = ?2, last_seen = max(last_seen, ?3) WHERE id = ?1",
                    params![id, text, r.started_at],
                )?;
                tx.execute("DELETE FROM sql_fts WHERE rowid = ?1", [id])?;
                tx.execute(
                    "INSERT INTO sql_fts(rowid, text, tables) VALUES (?1, ?2, ?3)",
                    params![id, text, tables],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO sql_texts(hash, text, fingerprint, kind, tables, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                    params![hash, text, fingerprint(&text), kind, tables, r.started_at],
                )?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO sql_fts(rowid, text, tables) VALUES (?1, ?2, ?3)",
                    params![id, text, tables],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO executions(hash, conn_key, conn_name, owner, worksheet, started_at, duration_ms, rows, status, error) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![hash, r.conn_key, r.conn_name, r.owner, r.worksheet, r.started_at, r.duration_ms, r.rows, r.status.as_str(), r.error.as_deref().map(redact_secrets)],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Add rows fetched after the first page (called as the user scrolls).
    pub fn add_rows(&mut self, execution_id: i64, more: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE executions SET rows = coalesce(rows, 0) + ?2 WHERE id = ?1",
            params![execution_id, more],
        )?;
        Ok(())
    }

    pub fn search(&self, q: &HistoryQuery, now_ms: i64) -> Result<Vec<HistoryHit>> {
        let fts = if q.text.trim().is_empty() {
            None
        } else {
            Some(
                fts_query(&q.text)
                    .ok_or_else(|| Error::Invalid("search text has no searchable words".into()))?,
            )
        };
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut sql = String::from(
            "SELECT t.hash, t.text, t.kind, t.tables, COUNT(e.id), SUM(e.status = 'ok'), MIN(e.started_at), MAX(e.started_at), \
                    AVG(CASE WHEN e.status = 'ok' THEN e.duration_ms END), t.pinned, ",
        );
        sql.push_str(if fts.is_some() {
            "m.snip, m.rank "
        } else {
            "NULL, 0.0 "
        });
        sql.push_str("FROM sql_texts t JOIN executions e ON e.hash = t.hash ");
        if let Some(f) = &fts {
            // snippet()/bm25() only work directly on the FTS table, so stage the matches first.
            self.conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS hist_match(rid INTEGER PRIMARY KEY, snip TEXT, rank REAL); DELETE FROM hist_match;")?;
            self.conn.execute(
                "INSERT INTO hist_match SELECT rowid, snippet(sql_fts, 0, '[', ']', '…', 14), bm25(sql_fts, 1.0, 3.0) FROM sql_fts WHERE sql_fts MATCH ?1",
                [f],
            )?;
            sql.push_str("JOIN hist_match m ON m.rid = t.id ");
        }
        sql.push_str("WHERE 1 = 1 ");
        let mut push = |clause: &str, v: rusqlite::types::Value, sql: &mut String| {
            args.push(v);
            sql.push_str(&clause.replace('?', &format!("?{}", args.len())));
        };
        if let Some(k) = &q.conn_key {
            push("AND e.conn_key = ? ", k.clone().into(), &mut sql);
        }
        if let Some(s) = q.since {
            push("AND e.started_at >= ? ", s.into(), &mut sql);
        }
        if let Some(u) = q.until {
            push("AND e.started_at <= ? ", u.into(), &mut sql);
        }
        if q.only_ok {
            sql.push_str("AND e.status = 'ok' ");
        }
        if q.pinned_only {
            sql.push_str("AND t.pinned = 1 ");
        }
        if !q.kinds.is_empty() {
            let marks: Vec<String> = q
                .kinds
                .iter()
                .map(|k| {
                    args.push(k.to_lowercase().into());
                    format!("?{}", args.len())
                })
                .collect();
            sql.push_str(&format!("AND t.kind IN ({}) ", marks.join(",")));
        }
        sql.push_str("GROUP BY t.hash ORDER BY MAX(e.started_at) DESC LIMIT 1500");

        let mut stmt = self.conn.prepare(&sql)?;
        let mut hits: Vec<HistoryHit> = stmt
            .query_map(params_from_iter(args.iter()), |r| {
                let last_run: i64 = r.get(7)?;
                let runs: i64 = r.get(4)?;
                let pinned = r.get::<_, i64>(9)? != 0;
                let rank: f64 = r.get(11)?;
                let age_days = ((now_ms - last_run).max(0) as f64) / 86_400_000.0;
                // bm25 is negative-better; blend relevance with how often and how recently it was run.
                let score = -rank
                    + 1.5 * (1.0 + runs as f64).ln()
                    + 2.0 * 0.5f64.powf(age_days / 30.0)
                    + if pinned { 5.0 } else { 0.0 };
                Ok(HistoryHit {
                    hash: r.get(0)?,
                    sql: r.get(1)?,
                    kind: r.get(2)?,
                    tables: r
                        .get::<_, String>(3)?
                        .split_whitespace()
                        .map(String::from)
                        .collect(),
                    runs,
                    ok_runs: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    first_run: r.get(6)?,
                    last_run,
                    avg_ms: r.get(8)?,
                    last_status: String::new(),
                    last_conn_name: String::new(),
                    last_conn_key: String::new(),
                    last_owner: String::new(),
                    pinned,
                    snippet: r.get(10)?,
                    score,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        if fts.is_some() {
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.last_run.cmp(&a.last_run))
            });
        } else {
            // browsing: pinned first, then newest
            hits.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.last_run.cmp(&a.last_run)));
        }
        let limit = q.limit.clamp(1, 200);
        let mut hits: Vec<HistoryHit> = hits.into_iter().skip(q.offset).take(limit).collect();
        // details of the most recent run, only for the rows we return
        let mut last = self
            .conn
            .prepare("SELECT status, conn_name, conn_key, owner FROM executions WHERE hash = ?1 ORDER BY started_at DESC, id DESC LIMIT 1")?;
        for h in hits.iter_mut() {
            let (s, n, k, o): (String, String, String, String) = last
                .query_row([&h.hash], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
            (
                h.last_status,
                h.last_conn_name,
                h.last_conn_key,
                h.last_owner,
            ) = (s, n, k, o);
        }
        Ok(hits)
    }

    /// Previously successful statements that continue what is being typed.
    /// Statements whose text starts with the typed text rank first.
    pub fn suggest(
        &self,
        typed: &str,
        conn_key: Option<&str>,
        limit: usize,
        now_ms: i64,
    ) -> Result<Vec<HistoryHit>> {
        if typed.trim().chars().count() < 3 {
            return Ok(vec![]);
        }
        let q = HistoryQuery {
            text: typed.to_string(),
            conn_key: conn_key.map(String::from),
            only_ok: true,
            limit: 50,
            ..Default::default()
        };
        let mut hits = match self.search(&q, now_ms) {
            Ok(h) => h,
            Err(Error::Invalid(_)) => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let typed_norm = collapse_upper(typed.trim().trim_end_matches(';'));
        for h in hits.iter_mut() {
            if collapse_upper(&h.sql).starts_with(&typed_norm) {
                h.score += 6.0;
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit.clamp(1, 20));
        Ok(hits)
    }

    pub fn set_pinned(&self, hash: &str, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE sql_texts SET pinned = ?2 WHERE hash = ?1",
            params![hash, pinned as i64],
        )?;
        Ok(())
    }

    /// Remove one query and all its executions.
    pub fn forget_query(&mut self, hash: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(id) = tx
            .query_row("SELECT id FROM sql_texts WHERE hash = ?1", [hash], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            tx.execute("DELETE FROM sql_fts WHERE rowid = ?1", [id])?;
            tx.execute("DELETE FROM sql_texts WHERE id = ?1", [id])?;
        }
        tx.execute("DELETE FROM executions WHERE hash = ?1", [hash])?;
        tx.commit()?;
        Ok(())
    }

    /// Delete executions older than `ts` and any query left with none (pinned queries stay).
    pub fn purge_before(&mut self, ts: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let n = tx.execute("DELETE FROM executions WHERE started_at < ?1", [ts])?;
        tx.execute("DELETE FROM sql_fts WHERE rowid IN (SELECT id FROM sql_texts WHERE pinned = 0 AND hash NOT IN (SELECT hash FROM executions))", [])?;
        tx.execute(
            "DELETE FROM sql_texts WHERE pinned = 0 AND hash NOT IN (SELECT hash FROM executions)",
            [],
        )?;
        tx.commit()?;
        Ok(n)
    }

    pub fn stats(&self) -> Result<HistoryStats> {
        let q = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(HistoryStats {
            executions: q("SELECT count(*) FROM executions")?,
            distinct_queries: q("SELECT count(*) FROM sql_texts")?,
            worksheets: q("SELECT count(*) FROM worksheets")?,
            first_at: self
                .conn
                .query_row("SELECT min(started_at) FROM executions", [], |r| r.get(0))?,
            size_bytes: q(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            )?,
        })
    }

    // ------------------------------------------------------------ worksheets

    fn outline(sql: &str) -> Vec<OutlineItem> {
        split_statements(sql)
            .into_iter()
            .map(|s| OutlineItem {
                kind: kind_name(s.kind).into(),
                tables: table_names(&s.text),
                preview: preview(&s.text),
                start: s.start,
                end: s.end,
            })
            .collect()
    }

    /// Create or update a worksheet (saving marks it open). Returns `None`, without
    /// creating anything, for a new worksheet that is still blank.
    pub fn save_worksheet(&mut self, w: &WorksheetSave, now_ms: i64) -> Result<Option<Worksheet>> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM worksheets WHERE uuid = ?1",
                [&w.uuid],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_none() && w.sql.trim().is_empty() {
            return Ok(None);
        }
        let outline = Self::outline(&w.sql);
        let outline_json =
            serde_json::to_string(&outline).map_err(|e| Error::Invalid(e.to_string()))?;
        let auto = auto_title(&w.sql);
        let custom = w
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(String::from);
        let tx = self.conn.transaction()?;
        let id = match existing {
            Some(id) => {
                // keep an earlier custom title unless a new one is given
                tx.execute(
                    "UPDATE worksheets SET conn_key=?2, conn_name=?3, owner=?4, title=coalesce(?5, title), auto_title=?6, sql=?7, outline=?8, updated_at=?9, closed_at=NULL, position=?10 WHERE id=?1",
                    params![id, w.conn_key, w.conn_name, w.owner, custom, auto, w.sql, outline_json, now_ms, w.position],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO worksheets(uuid, conn_key, conn_name, owner, title, auto_title, sql, outline, created_at, updated_at, position) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10)",
                    params![w.uuid, w.conn_key, w.conn_name, w.owner, custom, auto, w.sql, outline_json, now_ms, w.position],
                )?;
                tx.last_insert_rowid()
            }
        };
        let shown: String = tx.query_row(
            "SELECT coalesce(title, auto_title) FROM worksheets WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute("DELETE FROM ws_fts WHERE rowid = ?1", [id])?;
        tx.execute(
            "INSERT INTO ws_fts(rowid, title, sql) VALUES (?1, ?2, ?3)",
            params![id, shown, w.sql],
        )?;
        tx.commit()?;
        self.get_worksheet(&w.uuid)
    }

    pub fn close_worksheet(&self, uuid: &str, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE worksheets SET closed_at = ?2 WHERE uuid = ?1 AND closed_at IS NULL",
            params![uuid, now_ms],
        )?;
        Ok(())
    }

    pub fn set_worksheet_pinned(&self, uuid: &str, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE worksheets SET pinned = ?2 WHERE uuid = ?1",
            params![uuid, pinned as i64],
        )?;
        Ok(())
    }

    pub fn delete_worksheet(&mut self, uuid: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(id) = tx
            .query_row("SELECT id FROM worksheets WHERE uuid = ?1", [uuid], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
        {
            tx.execute("DELETE FROM ws_fts WHERE rowid = ?1", [id])?;
            tx.execute("DELETE FROM worksheets WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    fn row_to_ws(r: &rusqlite::Row<'_>) -> rusqlite::Result<Worksheet> {
        // column order is WS_COLS
        let custom: Option<String> = r.get(4)?;
        let auto: String = r.get(5)?;
        let outline: String = r.get(7)?;
        Ok(Worksheet {
            uuid: r.get(0)?,
            conn_key: r.get(1)?,
            conn_name: r.get(2)?,
            owner: r.get(3)?,
            custom_title: custom.is_some(),
            title: custom.unwrap_or(auto),
            sql: r.get(6)?,
            outline: serde_json::from_str(&outline).unwrap_or_default(),
            created_at: r.get(8)?,
            updated_at: r.get(9)?,
            closed_at: r.get(10)?,
            pinned: r.get::<_, i64>(11)? != 0,
            position: r.get(12)?,
            runs: r.get(13)?,
        })
    }

    const WS_COLS: &'static str = "w.uuid, w.conn_key, w.conn_name, w.owner, w.title, w.auto_title, w.sql, w.outline, w.created_at, w.updated_at, w.closed_at, w.pinned, w.position, \
         (SELECT count(*) FROM executions e WHERE e.worksheet = w.uuid)";

    pub fn get_worksheet(&self, uuid: &str) -> Result<Option<Worksheet>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {} FROM worksheets w WHERE w.uuid = ?1",
                    Self::WS_COLS
                ),
                [uuid],
                Self::row_to_ws,
            )
            .optional()?)
    }

    pub fn list_worksheets(&self, f: &WorksheetFilter) -> Result<Vec<Worksheet>> {
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut sql = format!("SELECT {} FROM worksheets w WHERE 1 = 1 ", Self::WS_COLS);
        match f.state {
            WsState::Open => sql.push_str("AND w.closed_at IS NULL "),
            WsState::Closed => sql.push_str("AND w.closed_at IS NOT NULL "),
            WsState::All => {}
        }
        if let Some(k) = &f.conn_key {
            args.push(k.clone().into());
            sql.push_str(&format!("AND w.conn_key = ?{} ", args.len()));
        }
        if !f.text.trim().is_empty() {
            let q = fts_query(&f.text)
                .ok_or_else(|| Error::Invalid("search text has no searchable words".into()))?;
            args.push(q.into());
            sql.push_str(&format!(
                "AND w.id IN (SELECT rowid FROM ws_fts WHERE ws_fts MATCH ?{}) ",
                args.len()
            ));
        }
        sql.push_str(if f.state == WsState::Open {
            "ORDER BY w.position, w.created_at "
        } else {
            "ORDER BY w.pinned DESC, w.updated_at DESC "
        });
        sql.push_str(&format!("LIMIT {}", f.limit.clamp(1, 500)));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(args.iter()), Self::row_to_ws)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// What was executed from a worksheet, in order: its step-by-step trail.
    pub fn worksheet_steps(&self, uuid: &str) -> Result<Vec<Step>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.started_at, e.hash, t.text, t.kind, e.status, e.duration_ms, e.rows, e.error \
             FROM executions e JOIN sql_texts t ON t.hash = e.hash WHERE e.worksheet = ?1 ORDER BY e.started_at, e.id",
        )?;
        let rows = stmt.query_map([uuid], |r| {
            Ok(Step {
                at: r.get(0)?,
                hash: r.get(1)?,
                sql: r.get(2)?,
                kind: r.get(3)?,
                status: r.get(4)?,
                duration_ms: r.get(5)?,
                rows: r.get(6)?,
                error: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_800_000_000_000;

    fn rec(sql: &str, at: i64) -> ExecRecord {
        ExecRecord {
            sql: sql.into(),
            conn_key: "k1".into(),
            conn_name: "HR · dev".into(),
            owner: "HR".into(),
            worksheet: None,
            started_at: at,
            duration_ms: 12,
            rows: Some(5),
            status: ExecStatus::Ok,
            error: None,
        }
    }

    fn q(text: &str) -> HistoryQuery {
        HistoryQuery {
            text: text.into(),
            limit: 50,
            ..Default::default()
        }
    }

    #[test]
    fn same_query_with_different_formatting_is_one_text_many_executions() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("select * from hr.emp where id = 5", NOW - 3 * DAY))
            .unwrap();
        h.record_execution(&rec("SELECT *\n  FROM hr.emp\n WHERE id = 5;", NOW - DAY))
            .unwrap();
        h.record_execution(&rec("select * from hr.emp where id = 6", NOW))
            .unwrap();
        let s = h.stats().unwrap();
        assert_eq!((s.executions, s.distinct_queries), (3, 2));
        let hits = h.search(&q(""), NOW).unwrap();
        assert_eq!(
            hits[0].sql, "select * from hr.emp where id = 6",
            "newest first"
        );
        let dup = hits.iter().find(|x| x.runs == 2).unwrap();
        assert_eq!(
            dup.sql, "SELECT *\n  FROM hr.emp\n WHERE id = 5",
            "stored copy keeps the latest formatting, without the trailing ;"
        );
        assert_eq!((dup.first_run, dup.last_run), (NOW - 3 * DAY, NOW - DAY));
        assert_eq!(dup.tables, vec!["HR.EMP"]);
    }

    #[test]
    fn finds_a_query_from_months_ago_by_words_tables_or_columns() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("select e.last_name, d.department_name from employees e join departments d on d.department_id = e.department_id where e.salary > 9000", NOW - 200 * DAY)).unwrap();
        h.record_execution(&rec("select count(*) from orders", NOW - 10 * DAY))
            .unwrap();
        for text in [
            "departments salary",
            "depart",
            "employees",
            "salary 9000",
            "last name",
        ] {
            let hits = h.search(&q(text), NOW).unwrap();
            assert!(
                hits.iter().any(|x| x.sql.contains("employees")),
                "{text}: {hits:?}"
            );
        }
        assert!(h.search(&q("nonexistentword"), NOW).unwrap().is_empty());
        let h1 = &h.search(&q("salary"), NOW).unwrap()[0];
        assert!(
            h1.snippet.as_ref().unwrap().contains("[salary]"),
            "{:?}",
            h1.snippet
        );
    }

    #[test]
    fn ranking_prefers_frequent_recent_and_pinned_over_a_stale_one_off() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec(
            "select * from emp where dept = 1 -- stale",
            NOW - 300 * DAY,
        ))
        .unwrap();
        for i in 0..6 {
            h.record_execution(&rec("select * from emp where dept = 2", NOW - i * DAY))
                .unwrap();
        }
        let hits = h.search(&q("emp"), NOW).unwrap();
        assert!(
            hits[0].sql.contains("dept = 2") && hits[0].runs == 6,
            "{hits:?}"
        );
        // pin the stale one: it moves up
        h.set_pinned(&hits[1].hash, true).unwrap();
        let hits = h.search(&q("emp"), NOW).unwrap();
        assert!(hits[0].pinned, "{hits:?}");
        assert_eq!(
            h.search(
                &HistoryQuery {
                    pinned_only: true,
                    limit: 10,
                    ..Default::default()
                },
                NOW
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn filters_connection_status_time_and_kind() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("select * from a", NOW - 50 * DAY))
            .unwrap();
        h.record_execution(&ExecRecord {
            conn_key: "k2".into(),
            conn_name: "SALES".into(),
            ..rec("select * from b", NOW - 2 * DAY)
        })
        .unwrap();
        h.record_execution(&ExecRecord {
            status: ExecStatus::Error,
            error: Some("ORA-00942".into()),
            ..rec("select * from missing", NOW - DAY)
        })
        .unwrap();
        h.record_execution(&rec("update a set x = 1 where y = 2", NOW))
            .unwrap();
        let n = |f: HistoryQuery| {
            h.search(&HistoryQuery { limit: 50, ..f }, NOW)
                .unwrap()
                .len()
        };
        assert_eq!(n(HistoryQuery::default()), 4);
        assert_eq!(
            n(HistoryQuery {
                conn_key: Some("k2".into()),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            n(HistoryQuery {
                only_ok: true,
                ..Default::default()
            }),
            3,
            "failed query hidden when only successful are wanted"
        );
        assert_eq!(
            n(HistoryQuery {
                since: Some(NOW - 7 * DAY),
                ..Default::default()
            }),
            3
        );
        assert_eq!(
            n(HistoryQuery {
                until: Some(NOW - 7 * DAY),
                ..Default::default()
            }),
            1
        );
        assert_eq!(
            n(HistoryQuery {
                kinds: vec!["update".into()],
                ..Default::default()
            }),
            1
        );
        let miss = h.search(&q("missing"), NOW).unwrap();
        assert_eq!(
            (miss[0].last_status.as_str(), miss[0].ok_runs),
            ("error", 0)
        );
    }

    #[test]
    fn last_run_details_and_average_time_cover_successful_runs_only() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&ExecRecord {
            duration_ms: 100,
            ..rec("select 1 from t", NOW - 2 * DAY)
        })
        .unwrap();
        h.record_execution(&ExecRecord {
            duration_ms: 300,
            ..rec("select 1 from t", NOW - DAY)
        })
        .unwrap();
        h.record_execution(&ExecRecord {
            duration_ms: 5000,
            status: ExecStatus::Cancelled,
            ..rec("select 1 from t", NOW)
        })
        .unwrap();
        let x = &h.search(&q(""), NOW).unwrap()[0];
        assert_eq!((x.runs, x.ok_runs, x.avg_ms), (3, 2, Some(200.0)));
        assert_eq!(
            (
                x.last_status.as_str(),
                x.last_conn_name.as_str(),
                x.last_owner.as_str()
            ),
            ("cancelled", "HR · dev", "HR")
        );
    }

    #[test]
    fn passwords_never_reach_the_history_file() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("create user bob identified by Hunter2Hunter2", NOW))
            .unwrap();
        h.record_execution(&ExecRecord {
            status: ExecStatus::Error,
            error: Some("ORA-01920 near identified by Hunter2Hunter2".into()),
            ..rec("alter user bob identified by Hunter2Hunter2", NOW)
        })
        .unwrap();
        assert!(
            h.search(&q("hunter2hunter2"), NOW).unwrap().is_empty(),
            "not even searchable"
        );
        let all = h.search(&q(""), NOW).unwrap();
        assert!(
            all.iter()
                .all(|x| !x.sql.contains("Hunter2") && x.sql.contains("identified by ***")),
            "{all:?}"
        );
        let raw: String = h
            .conn
            .query_row(
                "SELECT group_concat(coalesce(error,''), '|') FROM executions",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("Hunter2"), "{raw}");
    }

    #[test]
    fn suggestions_prefer_statements_that_start_with_what_is_typed() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec(
            "select first_name, salary from employees where department_id = 50",
            NOW - 20 * DAY,
        ))
        .unwrap();
        h.record_execution(&rec(
            "update employees set salary = salary * 1.1 where department_id = 50",
            NOW - 2 * DAY,
        ))
        .unwrap();
        h.record_execution(&ExecRecord {
            status: ExecStatus::Error,
            ..rec("select first_name from employeez", NOW)
        })
        .unwrap();
        let s = h.suggest("select first", Some("k1"), 5, NOW).unwrap();
        assert!(s[0].sql.starts_with("select first_name, salary"), "{s:?}");
        assert!(
            s.iter().all(|x| !x.sql.contains("employeez")),
            "failed queries are not suggested"
        );
        assert!(
            h.suggest("se", Some("k1"), 5, NOW).unwrap().is_empty(),
            "too short to suggest"
        );
        assert!(
            h.suggest("select first", Some("other"), 5, NOW)
                .unwrap()
                .is_empty(),
            "other connections are filtered"
        );
        assert!(
            h.suggest("\" ( * -", None, 5, NOW).unwrap().is_empty(),
            "unsearchable input is not an error"
        );
    }

    #[test]
    fn hostile_search_text_cannot_break_or_inject() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("select * from emp", NOW)).unwrap();
        for t in [
            "\"",
            "emp\" OR \"x",
            "emp*",
            "NEAR(a b)",
            "'; DROP TABLE executions; --",
            "name:emp",
            "(((",
            "😀",
        ] {
            let r = h.search(&q(t), NOW);
            assert!(
                r.is_ok() || matches!(r, Err(Error::Invalid(_))),
                "{t}: {r:?}"
            );
        }
        assert_eq!(h.stats().unwrap().executions, 1);
    }

    #[test]
    fn forget_and_purge_keep_the_indexes_consistent() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.record_execution(&rec("select * from old_t", NOW - 400 * DAY))
            .unwrap();
        h.record_execution(&rec("select * from pinned_t", NOW - 400 * DAY))
            .unwrap();
        h.record_execution(&rec("select * from new_t", NOW))
            .unwrap();
        let pin = h.search(&q("pinned"), NOW).unwrap()[0].hash.clone();
        h.set_pinned(&pin, true).unwrap();
        assert_eq!(h.purge_before(NOW - 365 * DAY).unwrap(), 2);
        let left: Vec<_> = h
            .search(&q(""), NOW)
            .unwrap()
            .into_iter()
            .map(|x| x.sql)
            .collect();
        assert_eq!(
            left,
            vec!["select * from new_t"],
            "pinned text survives but has no executions, so it is not listed"
        );
        assert!(h.search(&q("old_t"), NOW).unwrap().is_empty());
        let hash = h.search(&q("new_t"), NOW).unwrap()[0].hash.clone();
        h.forget_query(&hash).unwrap();
        assert!(h.search(&q("new_t"), NOW).unwrap().is_empty());
        assert_eq!(h.stats().unwrap().executions, 0);
    }

    fn ws(uuid: &str, sql: &str) -> WorksheetSave {
        WorksheetSave {
            uuid: uuid.into(),
            conn_key: "k1".into(),
            conn_name: "HR · dev".into(),
            owner: "HR".into(),
            title: None,
            sql: sql.into(),
            position: 0,
        }
    }

    #[test]
    fn worksheet_is_saved_outlined_titled_and_found_after_closing() {
        let mut h = HistoryStore::in_memory().unwrap();
        assert!(
            h.save_worksheet(&ws("w1", "   "), NOW).unwrap().is_none(),
            "blank new worksheets are not stored"
        );
        let w = h.save_worksheet(&ws("w1", "select * from hr.employees;\nupdate hr.employees set salary = 1 where id = 2;\n-- done"), NOW).unwrap().unwrap();
        assert_eq!(w.title, "SELECT HR.EMPLOYEES (+1 more)");
        assert_eq!(
            w.outline
                .iter()
                .map(|o| (o.kind.as_str(), o.tables.as_str()))
                .collect::<Vec<_>>(),
            vec![("select", "HR.EMPLOYEES"), ("update", "HR.EMPLOYEES")]
        );
        assert!(
            h.list_worksheets(&WorksheetFilter {
                state: WsState::Open,
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .len()
                == 1
        );
        h.close_worksheet("w1", NOW + 1000).unwrap();
        assert!(h
            .list_worksheets(&WorksheetFilter {
                state: WsState::Open,
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
        let closed = h
            .list_worksheets(&WorksheetFilter {
                state: WsState::Closed,
                text: "salary".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            closed[0].uuid, "w1",
            "closed worksheets stay searchable by their text"
        );
        // editing a closed worksheet reopens it
        let again = h
            .save_worksheet(&ws("w1", "select 1 from dual"), NOW + 2000)
            .unwrap()
            .unwrap();
        assert!(again.closed_at.is_none());
    }

    #[test]
    fn custom_title_sticks_until_replaced_and_autosave_is_idempotent() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.save_worksheet(
            &WorksheetSave {
                title: Some("Month-end checks".into()),
                ..ws("w", "select 1 from dual")
            },
            NOW,
        )
        .unwrap();
        let w = h
            .save_worksheet(&ws("w", "select 2 from dual"), NOW + 1)
            .unwrap()
            .unwrap();
        assert_eq!(
            (w.title.as_str(), w.custom_title),
            ("Month-end checks", true)
        );
        h.save_worksheet(&ws("w", "select 2 from dual"), NOW + 2)
            .unwrap();
        assert_eq!(
            h.list_worksheets(&WorksheetFilter {
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            h.list_worksheets(&WorksheetFilter {
                text: "month".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .len(),
            1,
            "title is searchable"
        );
        h.delete_worksheet("w").unwrap();
        assert!(h.get_worksheet("w").unwrap().is_none());
        assert!(h
            .list_worksheets(&WorksheetFilter {
                text: "month".into(),
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .is_empty());
    }

    #[test]
    fn worksheet_steps_are_the_ordered_trail_of_what_was_run() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.save_worksheet(
            &ws("w", "select * from a; update a set x = 1 where y = 2"),
            NOW,
        )
        .unwrap();
        let mk = |sql: &str, at: i64, status: ExecStatus| ExecRecord {
            worksheet: Some("w".into()),
            status,
            error: (status == ExecStatus::Error).then(|| "ORA-1".to_string()),
            ..rec(sql, at)
        };
        h.record_execution(&mk(
            "update a set x = 1 where y = 2",
            NOW + 20,
            ExecStatus::Error,
        ))
        .unwrap();
        h.record_execution(&mk("select * from a", NOW + 10, ExecStatus::Ok))
            .unwrap();
        h.record_execution(&rec("select * from other_sheet", NOW + 15))
            .unwrap();
        let steps = h.worksheet_steps("w").unwrap();
        assert_eq!(
            steps
                .iter()
                .map(|s| (s.kind.as_str(), s.status.as_str()))
                .collect::<Vec<_>>(),
            vec![("select", "ok"), ("update", "error")]
        );
        assert_eq!(steps[1].error.as_deref(), Some("ORA-1"));
        assert_eq!(h.get_worksheet("w").unwrap().unwrap().runs, 2);
    }

    #[test]
    fn open_worksheets_come_back_in_position_order() {
        let mut h = HistoryStore::in_memory().unwrap();
        h.save_worksheet(
            &WorksheetSave {
                position: 2,
                ..ws("c", "select 3 from dual")
            },
            NOW,
        )
        .unwrap();
        h.save_worksheet(
            &WorksheetSave {
                position: 0,
                ..ws("a", "select 1 from dual")
            },
            NOW + 1,
        )
        .unwrap();
        h.save_worksheet(
            &WorksheetSave {
                position: 1,
                ..ws("b", "select 2 from dual")
            },
            NOW + 2,
        )
        .unwrap();
        let open: Vec<_> = h
            .list_worksheets(&WorksheetFilter {
                state: WsState::Open,
                limit: 10,
                ..Default::default()
            })
            .unwrap()
            .into_iter()
            .map(|w| w.uuid)
            .collect();
        assert_eq!(open, vec!["a", "b", "c"]);
    }

    #[test]
    fn file_store_persists_across_reopen_and_rejects_newer_schema() {
        let dir = std::env::temp_dir().join(format!("ora-ide-hist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.sqlite");
        {
            let mut h = HistoryStore::open(&path).unwrap();
            h.record_execution(&rec("select * from persisted_t", NOW))
                .unwrap();
            h.save_worksheet(&ws("w", "select 1 from dual"), NOW)
                .unwrap();
        }
        {
            let h = HistoryStore::open(&path).unwrap();
            assert_eq!(h.search(&q("persisted_t"), NOW).unwrap().len(), 1);
            assert_eq!(
                h.list_worksheets(&WorksheetFilter {
                    limit: 5,
                    ..Default::default()
                })
                .unwrap()
                .len(),
                1
            );
        }
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", 99)
            .unwrap();
        assert!(matches!(HistoryStore::open(&path), Err(Error::Invalid(_))));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_year_of_history_searches_quickly() {
        let mut h = HistoryStore::in_memory().unwrap();
        for i in 0..20_000i64 {
            let sql = format!(
                "select col_{} , amount from sales_{} where region = 'R{}' and id > {}",
                i % 400,
                i % 60,
                i % 9,
                i
            );
            h.record_execution(&rec(&sql, NOW - (i % 365) * DAY))
                .unwrap();
        }
        let t0 = std::time::Instant::now();
        let hits = h.search(&q("sales_7 amount"), NOW).unwrap();
        let ms = t0.elapsed().as_millis();
        assert!(!hits.is_empty());
        assert!(ms < 500, "search over 20k executions took {ms} ms");
        println!("20k executions: search in {ms} ms, {} hits", hits.len());
    }
}
