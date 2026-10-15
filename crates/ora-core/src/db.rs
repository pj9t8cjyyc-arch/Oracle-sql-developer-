//! Backend-neutral database abstraction. Cells are display strings so the UI
//! never loses NUMBER precision; typed/columnar paths (Arrow) arrive with the
//! export phase.

use serde::Serialize;

use crate::Result;

/// `None` is SQL NULL.
pub type Cell = Option<String>;
pub type Row = Vec<Cell>;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
}

/// A server-side cursor that yields rows in batches.
pub trait Cursor: Send {
    fn columns(&self) -> &[Column];
    /// Up to `max_rows` rows. An empty vector means the cursor is exhausted.
    fn next_batch(&mut self, max_rows: usize) -> Result<Vec<Row>>;
}

pub enum Outcome {
    Rows(Box<dyn Cursor>),
    /// DML finished; number of affected rows. Not committed by the backend.
    Affected(u64),
    /// DDL / PL/SQL / other statement without a result set.
    Done,
}

/// Result of a bulk insert: rows that failed are reported by their index within the batch
/// and the others are inserted (not yet committed).
#[derive(Debug, Default, Clone)]
pub struct BatchOutcome {
    pub inserted: u64,
    pub errors: Vec<(usize, String)>,
}

pub trait Database: Send + Sync {
    /// Execute one statement (no trailing `;`). `fetch_size` is the driver
    /// array/prefetch size, the main lever for large-result throughput.
    fn run(&self, sql: &str, binds: &[String], fetch_size: usize) -> Result<Outcome>;
    fn commit(&self) -> Result<()>;
    fn rollback(&self) -> Result<()>;
    /// Insert many rows with one array-bound statement. Every row has one value per
    /// placeholder of `sql`; `None` binds NULL. Backends without bulk support return an error.
    fn insert_batch(&self, _sql: &str, _rows: &[Vec<Option<String>>]) -> Result<BatchOutcome> {
        Err(crate::Error::Invalid(
            "this backend does not support bulk insert".into(),
        ))
    }
    /// Interrupt the statement currently executing on this connection.
    fn cancel(&self) -> Result<()> {
        Ok(())
    }
}

/// Run a query and collect everything. Only for small metadata queries.
pub fn collect(db: &dyn Database, sql: &str, binds: &[String]) -> Result<(Vec<Column>, Vec<Row>)> {
    match db.run(sql, binds, 1000)? {
        Outcome::Rows(mut c) => {
            let cols = c.columns().to_vec();
            let mut rows = Vec::new();
            loop {
                let b = c.next_batch(1000)?;
                if b.is_empty() {
                    break;
                }
                rows.extend(b);
            }
            Ok((cols, rows))
        }
        _ => Ok((Vec::new(), Vec::new())),
    }
}
