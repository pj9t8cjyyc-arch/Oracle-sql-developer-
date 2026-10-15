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
    NeedsConfirmation { analysis: Analysis },
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
        /// `DBMS_OUTPUT` lines written by a PL/SQL block.
        output: Vec<String>,
    },
    Done {
        analysis: Analysis,
        output: Vec<String>,
        /// Set when a stored unit was created but does not compile.
        compile: Option<CompileReport>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CompileIssue {
    pub line: u32,
    pub column: u32,
    /// `ERROR` or `WARNING`.
    pub severity: String,
    pub text: String,
}

/// What `ALL_ERRORS` says about a stored unit that was just created.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CompileReport {
    /// `PROCEDURE HR.RAISE_PAY`
    pub object: String,
    pub issues: Vec<CompileIssue>,
    pub has_errors: bool,
}

/// Reads the compile errors of the unit `sql` just created. A failure to read them is not an error of the statement.
fn compile_report(db: &dyn Database, sql: &str) -> Option<CompileReport> {
    let unit = guard::stored_unit(sql)?;
    let (owner_sql, mut binds) = match &unit.owner {
        Some(o) => (":1".to_string(), vec![o.clone()]),
        None => (
            "SYS_CONTEXT('USERENV','CURRENT_SCHEMA')".to_string(),
            vec![],
        ),
    };
    let q = |n: usize| format!(":{n}");
    let base = binds.len();
    binds.push(unit.name.clone());
    binds.push(unit.kind.clone());
    let query = format!(
        "SELECT line, position, attribute, text FROM all_errors WHERE owner = {owner_sql} AND name = {} AND type = {} ORDER BY sequence",
        q(base + 1),
        q(base + 2)
    );
    let (_, rows) = crate::db::collect(db, &query, &binds).ok()?;
    if rows.is_empty() {
        return None;
    }
    let cell = |r: &Row, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
    let issues: Vec<CompileIssue> = rows
        .iter()
        .map(|r| CompileIssue {
            line: cell(r, 0).parse().unwrap_or(0),
            column: cell(r, 1).parse().unwrap_or(0),
            severity: cell(r, 2),
            text: cell(r, 3).trim().to_string(),
        })
        .collect();
    let has_errors = issues.iter().any(|i| i.severity == "ERROR");
    let object = match &unit.owner {
        Some(o) => format!("{} {o}.{}", unit.kind, unit.name),
        None => format!("{} {}", unit.kind, unit.name),
    };
    Some(CompileReport {
        object,
        issues,
        has_errors,
    })
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
            Outcome::Affected(count) => {
                let output = self.output_of(&analysis);
                Ok(RunResult::Affected {
                    count,
                    analysis,
                    output,
                })
            }
            Outcome::Done => {
                let output = self.output_of(&analysis);
                let compile = compile_report(self.db.as_ref(), &analysis.sql);
                Ok(RunResult::Done {
                    analysis,
                    output,
                    compile,
                })
            }
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

    /// `DBMS_OUTPUT` of a PL/SQL statement. Reading it is best effort: it never fails the statement.
    fn output_of(&self, analysis: &Analysis) -> Vec<String> {
        if analysis.kind == guard::StatementKind::PlSql {
            self.db.dbms_output().unwrap_or_default()
        } else {
            vec![]
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

    fn plsql_db() -> (QueryManager, Arc<Mutex<crate::fake::FakeLog>>) {
        let errors = FakeTable::new(
            &["LINE", "POSITION", "ATTRIBUTE", "TEXT"],
            vec![
                vec![
                    Some("3"),
                    Some("5"),
                    Some("ERROR"),
                    Some("PLS-00201: identifier 'X' must be declared "),
                ],
                vec![
                    Some("3"),
                    Some("5"),
                    Some("ERROR"),
                    Some("PL/SQL: Statement ignored"),
                ],
            ],
        );
        let db = FakeDb::new()
            .with_table("from all_errors", errors)
            .with_done("begin")
            .with_done("create or replace")
            .with_output(&["Updated 1 row(s)", "done"]);
        let log = db.log.clone();
        (QueryManager::new(Arc::new(db), Environment::Dev), log)
    }

    #[test]
    fn plsql_output_is_returned_with_the_result() {
        let (m, _) = plsql_db();
        let RunResult::Done {
            output, compile, ..
        } = m
            .run("begin dbms_output.put_line('x'); end;", opts(10, 10))
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(output, vec!["Updated 1 row(s)", "done"]);
        assert!(compile.is_none());
    }

    #[test]
    fn exec_is_sent_as_a_block() {
        let (m, log) = plsql_db();
        m.run("exec raise_pay(1, 5);", opts(10, 10)).unwrap();
        assert_eq!(
            log.lock().unwrap().calls[0].0,
            "BEGIN raise_pay(1, 5); END;"
        );
    }

    #[test]
    fn a_unit_that_does_not_compile_reports_its_errors_with_line_and_column() {
        let (m, log) = plsql_db();
        let RunResult::Done { compile, .. } = m
            .run(
                "create or replace procedure hr.p as begin x := 1; end;",
                opts(10, 10),
            )
            .unwrap()
        else {
            panic!()
        };
        let c = compile.expect("errors are reported");
        assert!(c.has_errors);
        assert_eq!(c.object, "PROCEDURE HR.P");
        assert_eq!((c.issues[0].line, c.issues[0].column), (3, 5));
        assert_eq!(
            c.issues[0].text,
            "PLS-00201: identifier 'X' must be declared"
        );
        let calls = log.lock().unwrap().calls.clone();
        let (q, binds) = calls.last().unwrap();
        assert!(
            q.contains("all_errors") && !q.contains("hr.p"),
            "names are bound, never pasted: {q}"
        );
        assert_eq!(
            binds,
            &vec!["HR".to_string(), "P".to_string(), "PROCEDURE".to_string()]
        );
    }

    #[test]
    fn a_unit_without_an_owner_is_looked_up_in_the_current_schema() {
        let (m, log) = plsql_db();
        m.run(
            "create or replace function f return number is begin return 1; end;",
            opts(10, 10),
        )
        .unwrap();
        let calls = log.lock().unwrap().calls.clone();
        let (q, binds) = calls.last().unwrap();
        assert!(q.contains("CURRENT_SCHEMA"), "{q}");
        assert_eq!(binds, &vec!["F".to_string(), "FUNCTION".to_string()]);
    }

    #[test]
    fn a_clean_compile_and_plain_ddl_report_nothing() {
        let db = FakeDb::new().with_done("create").with_table(
            "from all_errors",
            FakeTable::new(&["LINE", "POSITION", "ATTRIBUTE", "TEXT"], vec![]),
        );
        let m = QueryManager::new(Arc::new(db), Environment::Dev);
        for sql in [
            "create or replace procedure p as begin null; end;",
            "create table t (a number)",
        ] {
            let RunResult::Done {
                compile, output, ..
            } = m.run(sql, opts(10, 10)).unwrap()
            else {
                panic!()
            };
            assert!(compile.is_none() && output.is_empty(), "{sql}");
        }
    }
}
