//! Oracle backend on top of `rust-oracle` (ODPI-C). Needs Oracle Instant
//! Client at runtime.
//!
//! A `ResultSet` borrows its `Statement`, which borrows the `Connection`, so
//! it cannot be stored in a struct that outlives the call. Each open cursor
//! therefore lives on its own worker thread and is driven over a channel.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use oracle::{Connection, Connector};

use crate::db::{BatchOutcome, Column, Cursor, Database, Outcome, Row};
use crate::profile::ConnectionProfile;
use crate::{Error, Result};

fn err(e: oracle::Error) -> Error {
    Error::Database(e.to_string())
}

pub struct OracleDb {
    conn: Arc<Connection>,
}

impl OracleDb {
    pub fn connect(profile: &ConnectionProfile, password: &str) -> Result<Self> {
        profile.validate()?;
        let conn = Connector::new(&profile.user, password, profile.connect_string())
            .connect()
            .map_err(err)?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }
}

enum Reply {
    Columns(Vec<Column>),
    Affected(u64),
    Done,
    Failed(String),
}

enum Cmd {
    Fetch(usize, Sender<std::result::Result<Vec<Row>, String>>),
}

struct OracleCursor {
    columns: Vec<Column>,
    tx: Sender<Cmd>,
}

impl Cursor for OracleCursor {
    fn columns(&self) -> &[Column] {
        &self.columns
    }
    fn next_batch(&mut self, max_rows: usize) -> Result<Vec<Row>> {
        let (rtx, rrx) = channel();
        self.tx
            .send(Cmd::Fetch(max_rows, rtx))
            .map_err(|_| Error::Database("cursor worker stopped".into()))?;
        rrx.recv()
            .map_err(|_| Error::Database("cursor worker stopped".into()))?
            .map_err(Error::Database)
    }
}
// Dropping `OracleCursor` drops `tx`; the worker loop ends and releases the
// statement and result set.

fn cell(v: &oracle::SqlValue) -> Option<String> {
    match v.get::<Option<String>>() {
        Ok(c) => c,
        // BLOB/BFILE and other types that have no string form.
        Err(_) => Some("<unsupported type>".into()),
    }
}

fn worker(
    conn: Arc<Connection>,
    sql: String,
    binds: Vec<String>,
    fetch_size: usize,
    first: Sender<Reply>,
    cmds: Receiver<Cmd>,
) {
    let mut stmt = match conn
        .statement(&sql)
        .fetch_array_size(fetch_size as u32)
        .prefetch_rows(fetch_size as u32)
        .build()
    {
        Ok(s) => s,
        Err(e) => return drop(first.send(Reply::Failed(e.to_string()))),
    };
    let params: Vec<&dyn oracle::sql_type::ToSql> = binds
        .iter()
        .map(|b| b as &dyn oracle::sql_type::ToSql)
        .collect();
    if !stmt.is_query() {
        let reply = match stmt.execute(&params) {
            Ok(()) => match stmt.row_count() {
                Ok(n) if stmt.is_dml() => Reply::Affected(n),
                _ => Reply::Done,
            },
            Err(e) => Reply::Failed(e.to_string()),
        };
        return drop(first.send(reply));
    }
    let mut rs = match stmt.query(&params) {
        Ok(rs) => rs,
        Err(e) => return drop(first.send(Reply::Failed(e.to_string()))),
    };
    let cols = rs
        .column_info()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            data_type: c.oracle_type().to_string(),
            nullable: c.nullable(),
        })
        .collect();
    if first.send(Reply::Columns(cols)).is_err() {
        return;
    }
    while let Ok(Cmd::Fetch(n, reply)) = cmds.recv() {
        let mut rows = Vec::with_capacity(n.min(10_000));
        let mut failure = None;
        while rows.len() < n {
            match rs.next() {
                Some(Ok(row)) => rows.push(row.sql_values().iter().map(cell).collect()),
                Some(Err(e)) => {
                    failure = Some(e.to_string());
                    break;
                }
                None => break,
            }
        }
        let _ = reply.send(match failure {
            Some(f) => Err(f),
            None => Ok(rows),
        });
    }
}

impl Database for OracleDb {
    fn run(&self, sql: &str, binds: &[String], fetch_size: usize) -> Result<Outcome> {
        let (ftx, frx) = channel();
        let (ctx, crx) = channel();
        let (conn, sql, binds) = (self.conn.clone(), sql.to_string(), binds.to_vec());
        thread::Builder::new()
            .name("ora-cursor".into())
            .spawn(move || worker(conn, sql, binds, fetch_size.max(1), ftx, crx))
            .map_err(|e| Error::Database(e.to_string()))?;
        match frx
            .recv()
            .map_err(|_| Error::Database("cursor worker died".into()))?
        {
            Reply::Columns(columns) => {
                Ok(Outcome::Rows(Box::new(OracleCursor { columns, tx: ctx })))
            }
            Reply::Affected(n) => Ok(Outcome::Affected(n)),
            Reply::Done => Ok(Outcome::Done),
            Reply::Failed(m) => Err(Error::Database(m)),
        }
    }
    fn insert_batch(&self, sql: &str, rows: &[Vec<Option<String>>]) -> Result<BatchOutcome> {
        if rows.is_empty() {
            return Ok(BatchOutcome::default());
        }
        let mut b = self
            .conn
            .batch(sql, rows.len())
            .with_batch_errors()
            .build()
            .map_err(err)?;
        for r in rows {
            let params: Vec<&dyn oracle::sql_type::ToSql> = r
                .iter()
                .map(|c| c as &dyn oracle::sql_type::ToSql)
                .collect();
            b.append_row(&params).map_err(err)?;
        }
        // With batch errors, `execute` reports rows Oracle rejected as an `Err(BatchErrors)`
        // while every other row HAS been inserted: that is a partial success, not a failure.
        // (Needs Oracle client and server 12.1 or newer.)
        let errors: Vec<(usize, String)> = match b.execute() {
            Ok(()) => vec![],
            Err(e) => match e.batch_errors() {
                Some(errs) => {
                    let mut v: Vec<(usize, String)> = errs
                        .iter()
                        .map(|d| (d.offset() as usize, d.message().to_string()))
                        .collect();
                    v.sort_by_key(|(i, _)| *i);
                    v
                }
                None => return Err(err(e)),
            },
        };
        Ok(BatchOutcome {
            inserted: (rows.len() - errors.len()) as u64,
            errors,
        })
    }
    fn commit(&self) -> Result<()> {
        self.conn.commit().map_err(err)
    }
    fn rollback(&self) -> Result<()> {
        self.conn.rollback().map_err(err)
    }
    fn cancel(&self) -> Result<()> {
        self.conn.break_execution().map_err(err)
    }
}
