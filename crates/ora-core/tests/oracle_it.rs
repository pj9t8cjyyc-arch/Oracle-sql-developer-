//! Integration tests against a real Oracle database. Ignored by default.
//!
//!   ORA_HOST=localhost ORA_PORT=1521 ORA_SERVICE=FREEPDB1 ORA_USER=system ORA_PASSWORD=... \
//!   cargo test -p ora-core --features oracle --test oracle_it -- --ignored --nocapture
//!
//! Requires Oracle Instant Client on the library path. Tables are created in
//! the connecting user's schema and dropped afterwards.
#![cfg(feature = "oracle")]

use std::time::Instant;

use ora_core::db::{collect, Database};
use ora_core::oracle_backend::OracleDb;
use ora_core::profile::{ConnectionProfile, Environment};
use ora_core::schema;
use ora_core::session::{QueryManager, RunOptions, RunResult};
use std::sync::Arc;

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

fn connect() -> (OracleDb, ConnectionProfile) {
    let profile = ConnectionProfile {
        name: "it".into(),
        host: env("ORA_HOST", "localhost"),
        port: env("ORA_PORT", "1521").parse().unwrap(),
        service: env("ORA_SERVICE", "FREEPDB1"),
        user: env("ORA_USER", "system"),
        environment: Environment::Dev,
    };
    let db = OracleDb::connect(&profile, &env("ORA_PASSWORD", "")).expect("connect");
    (db, profile)
}

#[test]
#[ignore]
fn streams_a_large_result_in_pages_and_releases_the_cursor() {
    let (db, _) = connect();
    let m = QueryManager::new(Arc::new(db), Environment::Dev);
    let sql = "select level id, rpad('x', 50, 'y') payload from dual connect by level <= 200000";
    let t0 = Instant::now();
    let RunResult::Rows {
        handle,
        rows,
        done,
        columns,
        ..
    } = m
        .run(
            sql,
            RunOptions {
                page_size: 1000,
                ..Default::default()
            },
        )
        .unwrap()
    else {
        panic!("expected rows")
    };
    println!("first page: {} rows in {:?}", rows.len(), t0.elapsed());
    assert_eq!((rows.len(), done, columns.len()), (1000, false, 2));
    let mut total = rows.len();
    loop {
        let p = m.fetch_more(handle, 5000).unwrap();
        total += p.rows.len();
        if p.done {
            break;
        }
    }
    println!("total {total} rows in {:?}", t0.elapsed());
    assert_eq!(total, 100_000, "default max_rows cap of 100k applies");
    assert_eq!(m.open_cursors(), 0);
}

#[test]
#[ignore]
fn dml_is_not_committed_until_commit_and_rollback_works() {
    let (db, _) = connect();
    db.run(
        "create table ora_ide_it (id number(10), note varchar2(20 char))",
        &[],
        100,
    )
    .unwrap();
    let m = QueryManager::new(Arc::new(db), Environment::Dev);
    let go = RunOptions {
        confirmed: true,
        ..Default::default()
    };
    let r = m.run("insert into ora_ide_it values (1, 'a')", go).unwrap();
    assert!(matches!(r, RunResult::Affected { count: 1, .. }));
    m.rollback().unwrap();
    let RunResult::Rows { rows, .. } = m.run("select count(*) from ora_ide_it", go).unwrap() else {
        panic!()
    };
    assert_eq!(rows[0][0].as_deref(), Some("0"));
    m.run("drop table ora_ide_it purge", go).unwrap();
}

#[test]
#[ignore]
fn schema_browser_returns_columns_with_display_types() {
    let (db, profile) = connect();
    db.run("create table ora_ide_it2 (id number(10) not null, price number(10,2), name varchar2(30 char))", &[], 100).unwrap();
    let owner = profile.user.to_uppercase();
    let cols = schema::columns(&db, &owner, "ORA_IDE_IT2").unwrap();
    let got: Vec<_> = cols
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable))
        .collect();
    assert_eq!(
        got,
        vec![
            ("ID", "NUMBER(10)", false),
            ("PRICE", "NUMBER(10,2)", true),
            ("NAME", "VARCHAR2(30 CHAR)", true)
        ]
    );
    assert!(schema::objects(&db, &owner)
        .unwrap()
        .iter()
        .any(|o| o.name == "ORA_IDE_IT2"));
    assert!(schema::schemas(&db).unwrap().contains(&owner) || owner == "SYSTEM");
    db.run("drop table ora_ide_it2 purge", &[], 100).unwrap();
}

#[test]
#[ignore]
fn bad_sql_returns_ora_error_and_leaves_connection_usable() {
    let (db, _) = connect();
    let e = db
        .run("select * from no_such_table_xyz", &[], 100)
        .err()
        .expect("must fail");
    assert!(e.to_string().contains("ORA-00942"), "{e}");
    let (_, rows) = collect(&db, "select 1 from dual", &[]).unwrap();
    assert_eq!(rows.len(), 1);
}
