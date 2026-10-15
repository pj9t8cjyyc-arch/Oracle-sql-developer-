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
use crate::profile::{with_hint, AuthKind, ConnectionProfile, Privilege as ProfilePrivilege};
use crate::{Error, Result};

fn err(e: oracle::Error) -> Error {
    Error::Database(e.to_string())
}

pub struct OracleDb {
    conn: Arc<Connection>,
}

impl OracleDb {
    /// Connects as the profile says (host/service, TNS alias or connect string; password, wallet / OS
    /// or proxy login; optional SYSDBA/SYSOPER) and gives up after `connect_timeout_secs`.
    pub fn connect(profile: &ConnectionProfile, password: &str) -> Result<Self> {
        profile.validate()?;
        let cs = profile.resolved_connect_string()?;
        let admin = profile.tns_admin.trim();
        if !admin.is_empty() {
            // The Oracle client reads sqlnet.ora and wallets from TNS_ADMIN. It takes effect for
            // connections made after this; a different folder later in the same run needs a restart.
            let p = std::path::Path::new(admin);
            let dir = if p.is_file() {
                p.parent().unwrap_or(p)
            } else {
                p
            };
            std::env::set_var("TNS_ADMIN", dir);
        }
        let (user, pass) = (profile.login_user(), password.to_string());
        let external = profile.auth == AuthKind::External;
        let privilege = profile.privilege;
        let timeout = std::time::Duration::from_secs(profile.connect_timeout_secs);
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("ora-connect".into())
            .spawn(move || {
                let mut c = Connector::new(&user, &pass, cs);
                if external {
                    c.external_auth(true);
                }
                match privilege {
                    ProfilePrivilege::Sysdba => {
                        c.privilege(oracle::Privilege::Sysdba);
                    }
                    ProfilePrivilege::Sysoper => {
                        c.privilege(oracle::Privilege::Sysoper);
                    }
                    ProfilePrivilege::None => {}
                }
                let r = c.connect().inspect(|conn| {
                    // Lets a DBA recognise these sessions in V$SESSION.
                    let _ = conn.set_module("Ora IDE");
                    // Collect DBMS_OUTPUT so the IDE can show it; failing to enable is not fatal.
                    let _ = conn.execute("begin dbms_output.enable(1000000); end;", &[]);
                });
                let _ = tx.send(r.map_err(|e| e.to_string()));
            })
            .map_err(|e| Error::Database(format!("cannot start the connection: {e}")))?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(conn)) => Ok(Self {
                conn: Arc::new(conn),
            }),
            Ok(Err(e)) => Err(Error::Database(with_hint(&e))),
            Err(_) => Err(Error::Database(with_hint(&format!(
                "the connection attempt timed out after {} seconds (ORA-12170)",
                profile.connect_timeout_secs
            )))),
        }
    }
}

/// Version of the Oracle client library the app loaded, or the reason it could not be loaded.
pub fn client_version() -> std::result::Result<String, String> {
    oracle::Version::client()
        .map(|v| v.to_string())
        .map_err(|e| with_hint(&e.to_string()))
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
    fn dbms_output(&self) -> Result<Vec<String>> {
        use oracle::sql_type::OracleType;
        let mut stmt = self
            .conn
            .statement("begin dbms_output.get_line(:1, :2); end;")
            .build()
            .map_err(err)?;
        let mut lines = Vec::new();
        let mut bytes = 0usize;
        // Bounded so a runaway loop cannot fill memory.
        while lines.len() < 10_000 && bytes < 4_000_000 {
            stmt.execute(&[&OracleType::Varchar2(32767), &OracleType::Int64])
                .map_err(err)?;
            let status: i64 = stmt.bind_value(2).map_err(err)?;
            if status != 0 {
                break;
            }
            let line: Option<String> = stmt.bind_value(1).map_err(err)?;
            let line = line.unwrap_or_default();
            bytes += line.len();
            lines.push(line);
        }
        Ok(lines)
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
