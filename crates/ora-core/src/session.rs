//! Query sessions: safe-mode check, then run, then lazy paging over an open
//! server-side cursor so a huge result never has to be materialised.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db::{Column, Cursor, Database, Outcome, Row};
use crate::guard::{self, Analysis};
use crate::profile::Environment;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// Rows returned by the first call and by each `fetch_more`.
    pub page_size: usize,
    /// Driver array/prefetch size.
    pub fetch_size: usize,
    /// Hard cap on rows one query may deliver in total.
    pub max_rows: usize,
    /// The user acknowledged the safe-mode warnings.
    pub confirmed: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            page_size: 500,
            fetch_size: 5_000,
            max_rows: 100_000,
            confirmed: false,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunResult {
    /// Nothing was executed; ask the user, then re-run with `confirmed`.
    NeedsConfirmation {
        analysis: Analysis,
    },
    Rows {
        handle: u64,
        columns: Vec<Column>,
        rows: Vec<Row>,
        done: bool,
        analysis: Analysis,
    },
    Affected {
        count: u64,
        analysis: Analysis,
    },
    Done {
        analysis: Analysis,
    },
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Page {
    pub rows: Vec<Row>,
    pub done: bool,
    /// `max_rows` was hit before the result was exhausted.
    pub truncated: bool,
}

struct Open {
    cursor: Box<dyn Cursor>,
    delivered: usize,
    max_rows: usize,
}

pub struct QueryManager {
    db: Arc<dyn Database>,
    env: Environment,
    next_id: Mutex<u64>,
    open: Mutex<HashMap<u64, Arc<Mutex<Open>>>>,
}

impl QueryManager {
    pub fn new(db: Arc<dyn Database>, env: Environment) -> Self {
        Self {
            db,
            env,
            next_id: Mutex::new(1),
            open: Mutex::new(HashMap::new()),
        }
    }

    pub fn analyze(&self, sql: &str) -> Analysis {
        guard::analyze(sql, self.env)
    }

    pub fn run(&self, sql: &str, opts: RunOptions) -> Result<RunResult> {
        let analysis = self.analyze(sql);
        if analysis.kind == guard::StatementKind::Empty {
            return Err(Error::Invalid("empty statement".into()));
        }
        if let Some(why) = &analysis.blocked {
            return Err(Error::Blocked(why.clone()));
        }
        if analysis.requires_confirmation && !opts.confirmed {
            return Ok(RunResult::NeedsConfirmation { analysis });
        }
        match self.db.run(&analysis.sql, &[], opts.fetch_size)? {
            Outcome::Affected(count) => Ok(RunResult::Affected { count, analysis }),
            Outcome::Done => Ok(RunResult::Done { analysis }),
            Outcome::Rows(mut cursor) => {
                let columns = cursor.columns().to_vec();
                let first = opts.page_size.clamp(1, opts.max_rows.max(1));
                let rows = cursor.next_batch(first)?;
                let delivered = rows.len();
                // A short first page means the cursor is exhausted.
                let done = delivered < first || delivered >= opts.max_rows;
                let handle = {
                    let mut id = self.next_id.lock().unwrap();
                    let h = *id;
                    *id += 1;
                    h
                };
                if !done {
                    let open = Open {
                        cursor,
                        delivered,
                        max_rows: opts.max_rows,
                    };
                    self.open
                        .lock()
                        .unwrap()
                        .insert(handle, Arc::new(Mutex::new(open)));
                }
                Ok(RunResult::Rows {
                    handle,
                    columns,
                    rows,
                    done,
                    analysis,
                })
            }
        }
    }

    pub fn fetch_more(&self, handle: u64, page_size: usize) -> Result<Page> {
        let entry = self
            .open
            .lock()
            .unwrap()
            .get(&handle)
            .cloned()
            .ok_or(Error::UnknownHandle(handle))?;
        let mut open = entry.lock().unwrap();
        let want = page_size.max(1).min(open.max_rows - open.delivered);
        let rows = open.cursor.next_batch(want)?;
        open.delivered += rows.len();
        let exhausted = rows.len() < want;
        let capped = open.delivered >= open.max_rows;
        let done = exhausted || capped;
        drop(open);
        if done {
            self.open.lock().unwrap().remove(&handle);
        }
        Ok(Page {
            rows,
            done,
            truncated: capped && !exhausted,
        })
    }

    /// Release the server-side cursor. Idempotent.
    pub fn close(&self, handle: u64) {
        self.open.lock().unwrap().remove(&handle);
    }

    pub fn open_cursors(&self) -> usize {
        self.open.lock().unwrap().len()
    }

    pub fn cancel(&self) -> Result<()> {
        self.db.cancel()
    }
    pub fn commit(&self) -> Result<()> {
        self.db.commit()
    }
    pub fn rollback(&self) -> Result<()> {
        self.db.rollback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn big(n: usize) -> FakeTable {
        let rows: Vec<Vec<Option<String>>> = (0..n).map(|i| vec![Some(i.to_string())]).collect();
        let mut t = FakeTable::new(&["ID"], vec![]);
        t.rows = rows;
        t
    }

    fn mgr(n: usize, env: Environment) -> (QueryManager, Arc<Mutex<crate::fake::FakeLog>>) {
        let db = FakeDb::new()
            .with_table("from big", big(n))
            .with_affected(3);
        let log = db.log.clone();
        (QueryManager::new(Arc::new(db), env), log)
    }

    fn opts(page: usize, max: usize) -> RunOptions {
        RunOptions {
            page_size: page,
            max_rows: max,
            ..Default::default()
        }
    }

    #[test]
    fn pages_lazily_and_closes_cursor_when_done() {
        let (m, log) = mgr(1250, Environment::Dev);
        let RunResult::Rows {
            handle, rows, done, ..
        } = m.run("select * from big;", opts(500, 100_000)).unwrap()
        else {
            panic!()
        };
        assert_eq!((rows.len(), done), (500, false));
        assert_eq!(log.lock().unwrap().rows_served, 500, "must not read ahead");
        assert_eq!(m.open_cursors(), 1);

        let p = m.fetch_more(handle, 500).unwrap();
        assert_eq!((p.rows.len(), p.done), (500, false));
        let p = m.fetch_more(handle, 500).unwrap();
        assert_eq!((p.rows.len(), p.done, p.truncated), (250, true, false));
        assert_eq!(m.open_cursors(), 0);
        assert!(matches!(
            m.fetch_more(handle, 1),
            Err(Error::UnknownHandle(_))
        ));
    }

    #[test]
    fn exact_multiple_of_page_size_terminates() {
        let (m, _) = mgr(1000, Environment::Dev);
        let RunResult::Rows { handle, done, .. } =
            m.run("select * from big", opts(500, 100_000)).unwrap()
        else {
            panic!()
        };
        assert!(!done);
        assert!(!m.fetch_more(handle, 500).unwrap().done);
        let last = m.fetch_more(handle, 500).unwrap();
        assert!(last.rows.is_empty() && last.done);
        assert_eq!(m.open_cursors(), 0);
    }

    #[test]
    fn zero_page_size_cannot_wedge_a_cursor() {
        let (m, _) = mgr(5, Environment::Dev);
        let RunResult::Rows { rows, .. } = m.run("select * from big", opts(0, 100)).unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn small_result_never_leaves_cursor_open() {
        let (m, _) = mgr(10, Environment::Dev);
        let RunResult::Rows { done, .. } = m.run("select * from big", opts(500, 100_000)).unwrap()
        else {
            panic!()
        };
        assert!(done);
        assert_eq!(m.open_cursors(), 0);
    }

    #[test]
    fn max_rows_cap_truncates() {
        let (m, _) = mgr(10_000, Environment::Dev);
        let RunResult::Rows {
            handle, rows, done, ..
        } = m.run("select * from big", opts(400, 1000)).unwrap()
        else {
            panic!()
        };
        assert_eq!((rows.len(), done), (400, false));
        let p = m.fetch_more(handle, 400).unwrap();
        assert!(!p.truncated);
        let p = m.fetch_more(handle, 400).unwrap();
        assert_eq!((p.rows.len(), p.done, p.truncated), (200, true, true));
    }

    #[test]
    fn dangerous_statement_needs_confirmation_and_is_not_executed() {
        let (m, log) = mgr(1, Environment::Dev);
        let r = m.run("delete from emp", RunOptions::default()).unwrap();
        assert!(matches!(r, RunResult::NeedsConfirmation { .. }));
        assert!(log.lock().unwrap().calls.is_empty());
        let r = m
            .run(
                "delete from emp",
                RunOptions {
                    confirmed: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(matches!(r, RunResult::Affected { count: 3, .. }));
    }

    #[test]
    fn prod_dml_needs_confirmation_but_select_does_not() {
        let (m, _) = mgr(1, Environment::Prod);
        assert!(matches!(
            m.run("update t set a=1 where id=1", RunOptions::default())
                .unwrap(),
            RunResult::NeedsConfirmation { .. }
        ));
        assert!(matches!(
            m.run("select * from big", RunOptions::default()).unwrap(),
            RunResult::Rows { .. }
        ));
    }

    #[test]
    fn blocked_and_empty_are_errors_even_when_confirmed() {
        let (m, log) = mgr(1, Environment::Dev);
        let c = RunOptions {
            confirmed: true,
            ..Default::default()
        };
        assert!(matches!(
            m.run("select 1 from dual; delete from t", c),
            Err(Error::Blocked(_))
        ));
        assert!(matches!(
            m.run("-- only a comment", c),
            Err(Error::Invalid(_))
        ));
        assert!(log.lock().unwrap().calls.is_empty());
    }

    #[test]
    fn trailing_semicolon_is_stripped_before_reaching_driver() {
        let (m, log) = mgr(1, Environment::Dev);
        m.run("select * from big;  ", RunOptions::default())
            .unwrap();
        assert_eq!(log.lock().unwrap().calls[0].0, "select * from big");
    }
}
