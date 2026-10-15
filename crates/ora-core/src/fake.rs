//! In-memory [`Database`] used by unit tests and by the UI demo mode.

use std::sync::{Arc, Mutex};

use crate::db::{Column, Cursor, Database, Outcome, Row};
use crate::{Error, Result};

#[derive(Clone)]
pub struct FakeTable {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
}

impl FakeTable {
    pub fn new(cols: &[&str], rows: Vec<Vec<Option<&str>>>) -> Self {
        Self {
            columns: cols
                .iter()
                .map(|c| Column {
                    name: (*c).into(),
                    data_type: "VARCHAR2".into(),
                    nullable: true,
                })
                .collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(|c| c.map(String::from)).collect())
                .collect(),
        }
    }
}

#[derive(Default)]
pub struct FakeLog {
    /// (sql, binds) of every `run` call.
    pub calls: Vec<(String, Vec<String>)>,
    /// Rows handed out across all cursors, to prove lazy fetching.
    pub rows_served: usize,
    pub commits: usize,
    pub rollbacks: usize,
}

#[derive(Default)]
pub struct FakeDb {
    /// First responder whose key is a substring of the (lower-cased) SQL wins.
    responders: Vec<(String, FakeTable)>,
    affected: Option<u64>,
    pub log: Arc<Mutex<FakeLog>>,
}

impl FakeDb {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_table(mut self, sql_contains: &str, t: FakeTable) -> Self {
        self.responders.push((sql_contains.to_lowercase(), t));
        self
    }
    pub fn with_affected(mut self, n: u64) -> Self {
        self.affected = Some(n);
        self
    }
}

struct FakeCursor {
    columns: Vec<Column>,
    rows: std::vec::IntoIter<Row>,
    log: Arc<Mutex<FakeLog>>,
}

impl Cursor for FakeCursor {
    fn columns(&self) -> &[Column] {
        &self.columns
    }
    fn next_batch(&mut self, max_rows: usize) -> Result<Vec<Row>> {
        let batch: Vec<Row> = self.rows.by_ref().take(max_rows).collect();
        self.log.lock().unwrap().rows_served += batch.len();
        Ok(batch)
    }
}

impl Database for FakeDb {
    fn run(&self, sql: &str, binds: &[String], _fetch_size: usize) -> Result<Outcome> {
        self.log
            .lock()
            .unwrap()
            .calls
            .push((sql.to_string(), binds.to_vec()));
        let lower = sql.to_lowercase();
        if let Some((_, t)) = self.responders.iter().find(|(k, _)| lower.contains(k)) {
            return Ok(Outcome::Rows(Box::new(FakeCursor {
                columns: t.columns.clone(),
                rows: t.rows.clone().into_iter(),
                log: self.log.clone(),
            })));
        }
        if let Some(n) = self.affected {
            return Ok(Outcome::Affected(n));
        }
        Err(Error::Database(
            "ORA-00942: table or view does not exist".into(),
        ))
    }
    fn commit(&self) -> Result<()> {
        self.log.lock().unwrap().commits += 1;
        Ok(())
    }
    fn rollback(&self) -> Result<()> {
        self.log.lock().unwrap().rollbacks += 1;
        Ok(())
    }
}
