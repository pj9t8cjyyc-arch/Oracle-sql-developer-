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
    let mut profile = ConnectionProfile::easy(
        "it",
        &env("ORA_HOST", "localhost"),
        env("ORA_PORT", "1521").parse().unwrap(),
        &env("ORA_SERVICE", "FREEPDB1"),
        &env("ORA_USER", "system"),
    );
    profile.environment = Environment::Dev;
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

// ---------------------------------------------------------------- Phase 1
// These exercise the data-dictionary SQL, which unit tests can only fake.

fn exec(db: &OracleDb, sql: &str) {
    db.run(sql, &[], 100)
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn drop_quietly(db: &OracleDb, sql: &str) {
    let _ = db.run(sql, &[], 100);
}

#[test]
#[ignore]
fn insight_reports_keys_indexes_triggers_and_comments() {
    use ora_core::insight::table_insight;
    let (db, profile) = connect();
    drop_quietly(&db, "drop table ora_ide_emp purge");
    drop_quietly(&db, "drop table ora_ide_dept purge");
    exec(
        &db,
        "create table ora_ide_dept (id number(6) primary key, name varchar2(30))",
    );
    exec(&db, "create table ora_ide_emp (id number(6) primary key, email varchar2(50) unique, dept_id number(6) references ora_ide_dept(id), sal number(8,2) check (sal > 0), name varchar2(30) not null)");
    exec(
        &db,
        "create index ora_ide_emp_dept_sal on ora_ide_emp (dept_id, sal)",
    );
    exec(&db, "create or replace trigger ora_ide_emp_sal_trg before update of sal on ora_ide_emp for each row begin :new.sal := :new.sal; end;");
    exec(&db, "comment on column ora_ide_emp.sal is 'Monthly salary'");

    let owner = profile.user.to_uppercase();
    let i = table_insight(&db, &owner, "ORA_IDE_EMP").unwrap();
    let col = |n: &str| i.columns.iter().find(|c| c.name == n).unwrap().clone();

    assert!(col("ID").primary_key);
    assert!(col("EMAIL").unique);
    assert_eq!(col("DEPT_ID").foreign_keys[0].ref_table, "ORA_IDE_DEPT");
    assert_eq!(col("DEPT_ID").foreign_keys[0].ref_column, "ID");
    assert_eq!(col("SAL").checks, vec!["sal > 0".to_string()]);
    assert!(
        col("NAME").checks.is_empty(),
        "NOT NULL must not show as a check"
    );
    assert!(!col("NAME").nullable);
    assert_eq!(col("SAL").comment.as_deref(), Some("Monthly salary"));
    let sal = col("SAL");
    let ix = sal
        .indexes
        .iter()
        .find(|x| x.name == "ORA_IDE_EMP_DEPT_SAL")
        .expect("composite index");
    assert_eq!((ix.position, ix.leading), (2, false));
    assert!(col("DEPT_ID")
        .indexes
        .iter()
        .any(|x| x.name == "ORA_IDE_EMP_DEPT_SAL" && x.leading));
    let t = sal
        .triggers
        .iter()
        .find(|t| t.name == "ORA_IDE_EMP_SAL_TRG")
        .expect("UPDATE OF trigger on SAL");
    assert_eq!(t.reason, "update_of");
    assert!(
        col("EMAIL").triggers.is_empty(),
        "UPDATE OF SAL must not attach to other columns"
    );
    let parent = table_insight(&db, &owner, "ORA_IDE_DEPT").unwrap();
    assert!(parent
        .referenced_by
        .iter()
        .any(|c| c.table == "ORA_IDE_EMP"));

    drop_quietly(&db, "drop table ora_ide_emp purge");
    drop_quietly(&db, "drop table ora_ide_dept purge");
}

#[test]
#[ignore]
fn catalog_load_completion_and_search_work_end_to_end() {
    use ora_core::complete::{complete, Kind};
    use ora_core::meta::{load_schema, MetaCache};
    use ora_core::search::{docs_from_meta, SearchIndex, SearchQuery};
    let (db, profile) = connect();
    drop_quietly(&db, "drop table ora_ide_c_emp purge");
    drop_quietly(&db, "drop table ora_ide_c_dept purge");
    exec(
        &db,
        "create table ora_ide_c_dept (id number(6) primary key, dname varchar2(30))",
    );
    exec(&db, "create table ora_ide_c_emp (id number(6) primary key, dept_id number(6) references ora_ide_c_dept(id), ename varchar2(30))");
    exec(
        &db,
        "comment on table ora_ide_c_emp is 'Employees of the company'",
    );
    let owner = profile.user.to_uppercase();

    let t0 = Instant::now();
    let schema = load_schema(&db, &owner).unwrap();
    println!(
        "catalog load: {} tables in {:?}",
        schema.tables.len(),
        t0.elapsed()
    );
    assert!(schema.tables.contains_key("ORA_IDE_C_EMP"));
    assert!(schema
        .fks
        .iter()
        .any(|f| f.child_table == "ORA_IDE_C_EMP" && f.parent_table == "ORA_IDE_C_DEPT"));

    let mut cache = MetaCache {
        owners: vec![owner.clone()],
        ..Default::default()
    };
    cache.insert(schema.clone());
    let sql = "select * from ora_ide_c_emp e join ";
    let c = complete(&cache, &owner, sql, sql.len());
    assert!(
        c.items.iter().any(|s| s.kind == Kind::Join
            && s.insert.contains("ORA_IDE_C_DEPT")
            && s.insert.contains("DEPT_ID")),
        "{:?}",
        c.items.iter().take(5).collect::<Vec<_>>()
    );
    let sql = "select e. from ora_ide_c_emp e";
    let c = complete(&cache, &owner, sql, 9);
    assert!(c.items.iter().any(|s| s.label == "ENAME"));

    let mut idx = SearchIndex::in_memory().unwrap();
    idx.replace_owner(&owner, &docs_from_meta(&schema)).unwrap();
    let hits = idx
        .search(&SearchQuery {
            text: "employees company".into(),
            limit: 5,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits[0].name, "ORA_IDE_C_EMP");

    drop_quietly(&db, "drop table ora_ide_c_emp purge");
    drop_quietly(&db, "drop table ora_ide_c_dept purge");
}

#[test]
#[ignore]
fn saving_comments_round_trips_including_quotes() {
    use ora_core::comments::save_comment;
    let (db, profile) = connect();
    drop_quietly(&db, "drop table ora_ide_cm purge");
    exec(&db, "create table ora_ide_cm (a number)");
    let owner = profile.user.to_uppercase();
    let text = "it's a 'test'; DROP TABLE x; --";
    save_comment(&db, &owner, "ORA_IDE_CM", Some("A"), text).unwrap();
    let (_, rows) = collect(&db, "select comments from all_col_comments where owner = :1 and table_name = 'ORA_IDE_CM' and column_name = 'A'", std::slice::from_ref(&owner)).unwrap();
    assert_eq!(rows[0][0].as_deref(), Some(text));
    save_comment(&db, &owner, "ORA_IDE_CM", None, "").unwrap();
    let (_, rows) = collect(
        &db,
        "select comments from all_tab_comments where owner = :1 and table_name = 'ORA_IDE_CM'",
        &[owner],
    )
    .unwrap();
    assert!(rows[0][0].is_none(), "empty text clears the comment");
    drop_quietly(&db, "drop table ora_ide_cm purge");
}

// ------------------------------------------------- dictionary browser + sequences

#[test]
#[ignore]
fn sequences_are_found_via_trigger_source_and_identity_columns() {
    use ora_core::insight::table_insight;
    let (db, profile) = connect();
    for s in [
        "drop table ora_ide_s purge",
        "drop sequence ora_ide_s_seq",
        "drop sequence ora_ide_s_dflt",
    ] {
        drop_quietly(&db, s);
    }
    exec(
        &db,
        "create sequence ora_ide_s_seq start with 100 increment by 5 cache 20",
    );
    exec(&db, "create sequence ora_ide_s_dflt");
    // identity needs 12c+; DEFAULT seq.NEXTVAL needs 12c+ too
    exec(&db, "create table ora_ide_s (id number primary key, code number generated always as identity, ref number default ora_ide_s_dflt.nextval, note varchar2(10))");
    exec(&db, "create or replace trigger ora_ide_s_bi before insert on ora_ide_s for each row begin :new.id := ora_ide_s_seq.nextval; end;");

    let owner = profile.user.to_uppercase();
    let t = table_insight(&db, &owner, "ORA_IDE_S").unwrap();
    let by = |n: &str| {
        t.sequences
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| {
                panic!(
                    "{n} not found in {:?}",
                    t.sequences
                        .iter()
                        .map(|s| (&s.name, &s.via))
                        .collect::<Vec<_>>()
                )
            })
            .clone()
    };

    let trg = by("ORA_IDE_S_SEQ");
    assert_eq!(
        (trg.via.as_str(), trg.via_name.as_str(), trg.columns.clone()),
        ("trigger", "ORA_IDE_S_BI", vec!["ID".to_string()])
    );
    assert_eq!(
        (trg.increment.as_deref(), trg.cache.as_deref()),
        (Some("5"), Some("20"))
    );
    assert!(t
        .columns
        .iter()
        .find(|c| c.name == "ID")
        .unwrap()
        .sequences
        .iter()
        .any(|q| q.name == "ORA_IDE_S_SEQ"));
    assert_eq!(by("ORA_IDE_S_DFLT").via, "default");
    let ident = t
        .sequences
        .iter()
        .find(|s| s.via == "identity")
        .expect("identity column");
    assert_eq!(ident.columns, vec!["CODE".to_string()]);
    assert_eq!(t.triggers[0].sequences, vec!["ORA_IDE_S_SEQ".to_string()]);

    for s in [
        "drop table ora_ide_s purge",
        "drop sequence ora_ide_s_seq",
        "drop sequence ora_ide_s_dflt",
    ] {
        drop_quietly(&db, s);
    }
}

#[test]
#[ignore]
fn dictionary_browser_lists_kinds_and_returns_source_ddl_and_dependencies() {
    use ora_core::catalog::{
        dependencies, list_objects, object_counts, object_ddl, object_source, sequence_info,
    };
    let (db, profile) = connect();
    for s in [
        "drop table ora_ide_b purge",
        "drop sequence ora_ide_b_seq",
        "drop procedure ora_ide_b_proc",
    ] {
        drop_quietly(&db, s);
    }
    exec(&db, "create sequence ora_ide_b_seq");
    exec(
        &db,
        "create table ora_ide_b (id number primary key, v varchar2(10))",
    );
    exec(&db, "create index ora_ide_b_ix on ora_ide_b (v)");
    exec(&db, "create or replace trigger ora_ide_b_trg before insert on ora_ide_b for each row begin :new.id := ora_ide_b_seq.nextval; end;");
    exec(&db, "create or replace procedure ora_ide_b_proc as begin insert into ora_ide_b (v) values ('x'); end;");
    let owner = profile.user.to_uppercase();

    let counts = object_counts(&db, &owner).unwrap();
    for k in ["TABLE", "INDEX", "SEQUENCE", "TRIGGER", "PROCEDURE"] {
        assert!(
            counts.get(k).copied().unwrap_or(0) >= 1,
            "{k} missing in {counts:?}"
        );
    }
    let idx = list_objects(&db, &owner, "INDEX").unwrap();
    let ix = idx
        .iter()
        .find(|e| e.name == "ORA_IDE_B_IX")
        .expect("index listed");
    assert_eq!(ix.parent.as_deref(), Some("ORA_IDE_B"));
    assert!(ix.ok);
    let trg = list_objects(&db, &owner, "TRIGGER").unwrap();
    assert_eq!(
        trg.iter()
            .find(|e| e.name == "ORA_IDE_B_TRG")
            .unwrap()
            .parent
            .as_deref(),
        Some("ORA_IDE_B")
    );
    assert!(list_objects(&db, &owner, "SEQUENCE")
        .unwrap()
        .iter()
        .any(|e| e.name == "ORA_IDE_B_SEQ"));
    assert!(
        list_objects(&db, &owner, "SEQUENCE")
            .unwrap()
            .iter()
            .all(|e| !e.name.starts_with("ISEQ$$")),
        "identity helpers hidden"
    );

    let src = object_source(&db, &owner, "ORA_IDE_B_PROC", "PROCEDURE").unwrap();
    assert!(
        src.to_lowercase().contains("insert into ora_ide_b"),
        "{src}"
    );
    let ddl = object_ddl(&db, &owner, "ORA_IDE_B", "TABLE").unwrap();
    assert!(ddl.to_uppercase().contains("CREATE TABLE"), "{ddl}");
    let seq = sequence_info(&db, &owner, "ORA_IDE_B_SEQ")
        .unwrap()
        .expect("sequence info");
    assert_eq!(seq.increment, "1");

    let d = dependencies(&db, &owner, "ORA_IDE_B_SEQ").unwrap();
    assert!(
        d.used_by
            .iter()
            .any(|x| x.name == "ORA_IDE_B_TRG" && x.object_type == "TRIGGER"),
        "{d:?}"
    );
    let d = dependencies(&db, &owner, "ORA_IDE_B_PROC").unwrap();
    assert!(d.uses.iter().any(|x| x.name == "ORA_IDE_B"), "{d:?}");

    for s in [
        "drop table ora_ide_b purge",
        "drop sequence ora_ide_b_seq",
        "drop procedure ora_ide_b_proc",
    ] {
        drop_quietly(&db, s);
    }
}

// ------------------------------------------------------------- export / import

const EXPORT_NLS: &str = "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM' NLS_NUMERIC_CHARACTERS = '.,'";

#[test]
#[ignore]
fn export_writes_iso_dates_and_dot_decimals_whatever_the_session_uses() {
    use ora_core::db::Outcome;
    use ora_core::export::{export, ExportOptions, Format};
    use std::sync::atomic::AtomicBool;
    let (db, _) = connect();
    // a hostile session: comma decimals and a long date format
    exec(
        &db,
        "ALTER SESSION SET NLS_NUMERIC_CHARACTERS = ',.' NLS_DATE_FORMAT = 'DD-MON-RR'",
    );
    exec(&db, EXPORT_NLS);
    let sql = "select 1234.5 amt, .5 half, to_date('2020-02-29 13:45:10','YYYY-MM-DD HH24:MI:SS') d, to_timestamp('2020-02-29 13:45:10.123456','YYYY-MM-DD HH24:MI:SS.FF6') ts, 'x' || chr(10) || 'y' txt from dual";
    let Outcome::Rows(mut cur) = db.run(sql, &[], 100).unwrap() else {
        panic!("rows expected")
    };
    let dir = std::env::temp_dir().join(format!("ora-ide-it-exp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rep = export(
        cur.as_mut(),
        &dir.join("out"),
        &ExportOptions {
            format: Format::Csv,
            bom: false,
            ..Default::default()
        },
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let text = std::fs::read_to_string(&rep.files[0]).unwrap();
    println!("{text}");
    assert!(
        text.contains("1234.5") && text.contains("0.5") || text.contains(".5"),
        "dot decimals: {text}"
    );
    assert!(
        text.contains("2020-02-29 13:45:10,2020-02-29 13:45:10.123456"),
        "ISO date and timestamp: {text}"
    );
    assert!(
        text.contains("\"x\ny\"") || text.contains("\"x\r\ny\""),
        "embedded newline is quoted: {text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore]
fn import_loads_rows_with_explicit_formats_and_maps_oracle_rejects_to_file_rows() {
    use ora_core::import::plan::{import, load_targets, plan_mapping, ImportOptions, PlanInput};
    use ora_core::import::profile::profile_source;
    use ora_core::import::source::{open_source, ReadOptions};
    use std::sync::atomic::AtomicBool;
    let (db, profile) = connect();
    let owner = profile.user.to_uppercase();
    drop_quietly(&db, "drop table ora_ide_imp purge");
    exec(&db, "create table ora_ide_imp (id number(10) primary key, name varchar2(10 char), amount number(8,2), born date, seen timestamp(6))");
    // session NLS must not matter: use a hostile one
    exec(
        &db,
        "ALTER SESSION SET NLS_NUMERIC_CHARACTERS = ',.' NLS_DATE_FORMAT = 'DD-MON-RR'",
    );

    let dir = std::env::temp_dir().join(format!("ora-ide-it-imp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("in.csv");
    // row 3 repeats id 1 (unique violation, reported by Oracle); row 4 is too long for NAME (caught before the database)
    std::fs::write(&f, "id,name,amount,born,seen\n1,Ann,1234.50,31/01/2020,2020-01-31 10:20:30.5\n2,Bob,.5,15/02/2021,2021-02-15 00:00:00\n1,Dup,1,01/03/2020,2020-03-01 00:00:00\n4,Waaaaaaaay too long,2,01/03/2020,2020-03-01 00:00:00\n").unwrap();
    let prof = profile_source(
        &mut *open_source(&f, &ReadOptions::default()).unwrap(),
        1000,
    )
    .unwrap();
    let targets = load_targets(&db, &owner, "ORA_IDE_IMP").unwrap();
    let plan = plan_mapping(
        &owner,
        "ORA_IDE_IMP",
        &prof,
        &targets,
        &PlanInput::default(),
    );
    let rej = dir.join("in.rejects.csv");
    let r = import(
        &db,
        &mut *open_source(&f, &ReadOptions::default()).unwrap(),
        &plan,
        &ImportOptions::default(),
        &rej,
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    println!("{r:?}");
    assert_eq!(
        (r.read, r.inserted, r.rejected, r.committed),
        (4, 2, 2, true),
        "{:?}",
        r.samples
    );
    let rejects = std::fs::read_to_string(&rej).unwrap();
    assert!(
        rejects.contains("\n3,") && rejects.contains("ORA-00001"),
        "Oracle's unique-constraint error is attributed to file row 3: {rejects}"
    );
    assert!(
        rejects.contains("\n4,") && rejects.contains("too long"),
        "{rejects}"
    );
    let (_, rows) = collect(&db, "select id, name, to_char(amount,'TM9','NLS_NUMERIC_CHARACTERS=''.,'''), to_char(born,'YYYY-MM-DD'), to_char(seen,'YYYY-MM-DD HH24:MI:SS.FF1') from ora_ide_imp order by id", &[]).unwrap();
    assert_eq!(
        rows[0],
        vec![
            Some("1".into()),
            Some("Ann".into()),
            Some("1234.5".into()),
            Some("2020-01-31".into()),
            Some("2020-01-31 10:20:30.5".into())
        ]
    );
    assert_eq!(rows[1][2].as_deref(), Some(".5"));
    assert_eq!(
        rows[1][3].as_deref(),
        Some("2021-02-15"),
        "day-first dates read correctly regardless of session NLS_DATE_FORMAT"
    );
    drop_quietly(&db, "drop table ora_ide_imp purge");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore]
fn package_analysis_and_dependency_graph_match_a_known_schema() {
    let (db, p) = connect();
    let owner = p.user.to_uppercase();
    for t in ["IT_PK_T", "IT_PK_T2"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
    drop_quietly(&db, "DROP PACKAGE it_pk_a");
    drop_quietly(&db, "DROP PACKAGE it_pk_b");
    exec(
        &db,
        "CREATE TABLE it_pk_t (id NUMBER PRIMARY KEY, v VARCHAR2(10))",
    );
    exec(
        &db,
        "CREATE TABLE it_pk_t2 (id NUMBER, t_id NUMBER REFERENCES it_pk_t(id))",
    );
    exec(&db, "CREATE OR REPLACE PACKAGE it_pk_b AS PROCEDURE ping(p_x IN NUMBER); FUNCTION cnt RETURN NUMBER; END it_pk_b;");
    exec(&db, "CREATE OR REPLACE PACKAGE BODY it_pk_b AS PROCEDURE ping(p_x IN NUMBER) IS BEGIN DELETE FROM it_pk_t2 WHERE id = p_x; END; FUNCTION cnt RETURN NUMBER IS n NUMBER; BEGIN SELECT COUNT(*) INTO n FROM it_pk_t; RETURN n; END; END it_pk_b;");
    exec(
        &db,
        "CREATE OR REPLACE PACKAGE it_pk_a AS PROCEDURE run_it; END it_pk_a;",
    );
    exec(&db, "CREATE OR REPLACE PACKAGE BODY it_pk_a AS PROCEDURE run_it IS BEGIN it_pk_b.ping(1); INSERT INTO it_pk_t(id) VALUES (1); END; END it_pk_a;");

    let a = ora_core::packages::analyze_schema(&db, &owner).unwrap();
    let pa = a
        .packages
        .iter()
        .find(|x| x.name == "IT_PK_A")
        .expect("IT_PK_A");
    let pb = a
        .packages
        .iter()
        .find(|x| x.name == "IT_PK_B")
        .expect("IT_PK_B");
    assert_eq!(pa.calls_packages, vec!["IT_PK_B"]);
    assert_eq!(pb.called_by, vec!["IT_PK_A"]);
    assert!(pa.tables.iter().any(|t| t.table == "IT_PK_T" && t.write));
    let ping = pb.members.iter().find(|m| m.name == "PING").unwrap();
    assert_eq!(ping.signature, "PING(P_X IN NUMBER)");
    assert_eq!(ping.status, "used");
    let cnt = pb.members.iter().find(|m| m.name == "CNT").unwrap();
    assert_eq!(cnt.signature, "CNT RETURN NUMBER");
    assert_eq!(cnt.status, "unreferenced");

    let g = ora_core::graph::load(&db, &owner).unwrap();
    let focus = ora_core::graph::Focus {
        id: format!("TABLE:{owner}.IT_PK_T"),
        direction: ora_core::graph::Direction::UsedBy,
        hops: 2,
        kinds: None,
        max_nodes: None,
    };
    let s = g.focus(&focus).expect("table in graph");
    let names: Vec<&str> = s.nodes.iter().map(|n| n.node.name.as_str()).collect();
    assert!(names.contains(&"IT_PK_T2"), "foreign key child: {names:?}");
    assert!(
        names.contains(&"IT_PK_A") && names.contains(&"IT_PK_B"),
        "packages: {names:?}"
    );
    drop_quietly(&db, "DROP PACKAGE it_pk_a");
    drop_quietly(&db, "DROP PACKAGE it_pk_b");
    for t in ["IT_PK_T2", "IT_PK_T"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
}

#[test]
#[ignore]
fn knowledge_base_and_markdown_dossier_cover_a_known_schema() {
    use ora_core::dossier::{build, DbFetch, DossierOptions};
    use ora_core::graph::Direction;
    let (db, p) = connect();
    let owner = p.user.to_uppercase();
    for sql in [
        "DROP TRIGGER it_kb_bi",
        "DROP PACKAGE it_kb_pkg",
        "DROP SEQUENCE it_kb_seq",
    ] {
        drop_quietly(&db, sql);
    }
    for t in ["IT_KB_CHILD", "IT_KB_PARENT"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
    exec(
        &db,
        "CREATE TABLE it_kb_parent (id NUMBER PRIMARY KEY, name VARCHAR2(30))",
    );
    exec(
        &db,
        "COMMENT ON TABLE it_kb_parent IS 'knowledge test parent'",
    );
    exec(&db, "CREATE TABLE it_kb_child (id NUMBER PRIMARY KEY, parent_id NUMBER REFERENCES it_kb_parent(id))");
    exec(&db, "CREATE INDEX it_kb_child_ix ON it_kb_child(parent_id)");
    exec(&db, "CREATE SEQUENCE it_kb_seq");
    exec(&db, "CREATE OR REPLACE TRIGGER it_kb_bi BEFORE INSERT ON it_kb_parent FOR EACH ROW BEGIN :new.id := it_kb_seq.NEXTVAL; END;");
    exec(&db, "CREATE OR REPLACE PACKAGE it_kb_pkg AS PROCEDURE add_child(p_parent NUMBER); END it_kb_pkg;");
    exec(&db, "CREATE OR REPLACE PACKAGE BODY it_kb_pkg AS\n  PROCEDURE add_child(p_parent NUMBER) IS\n  BEGIN\n    INSERT INTO it_kb_child(id, parent_id) VALUES (1, p_parent);\n  END;\nEND it_kb_pkg;");

    let kb = ora_core::knowledge::collect_schema(&db, &owner, 0).unwrap();
    let parent = kb.table("IT_KB_PARENT").expect("parent table");
    assert_eq!(parent.comment.as_deref(), Some("knowledge test parent"));
    assert_eq!(parent.columns.len(), 2);
    let child = kb.table("IT_KB_CHILD").expect("child table");
    assert!(child
        .constraints
        .iter()
        .any(|c| c.kind == "R"
            && c.ref_table.as_deref() == Some(&format!("{owner}.IT_KB_PARENT")[..])));
    assert!(child
        .indexes
        .iter()
        .any(|i| i.name == "IT_KB_CHILD_IX" && i.columns == ["PARENT_ID"]));
    assert!(kb
        .triggers
        .iter()
        .any(|t| t.name == "IT_KB_BI" && t.table.as_deref() == Some("IT_KB_PARENT")));
    let edge = |a: &str, b: &str| {
        kb.edges
            .iter()
            .find(|e| e.from == a && e.to == b)
            .map(|e| e.kind.as_str())
    };
    assert_eq!(
        edge(
            &format!("TABLE:{owner}.IT_KB_CHILD"),
            &format!("TABLE:{owner}.IT_KB_PARENT")
        ),
        Some("fk")
    );
    assert_eq!(
        edge(
            &format!("PACKAGE:{owner}.IT_KB_PKG"),
            &format!("TABLE:{owner}.IT_KB_CHILD")
        ),
        Some("writes")
    );
    assert_eq!(
        edge(
            &format!("INDEX:{owner}.IT_KB_CHILD_IX"),
            &format!("TABLE:{owner}.IT_KB_CHILD")
        ),
        Some("indexes")
    );

    let b = build(
        &kb,
        &DbFetch(&db),
        &DossierOptions {
            focus: format!("TABLE:{owner}.IT_KB_CHILD"),
            direction: Direction::Both,
            hops: 2,
            include_source: true,
            max_member_lines: 0,
            max_nodes: None,
        },
    )
    .unwrap();
    let md = b.combined();
    assert!(
        md.contains("IT_KB_CHILD_IX") && md.contains("INSERT INTO it_kb_child"),
        "{md}"
    );
    assert!(
        md.to_uppercase().contains("CREATE TABLE"),
        "DDL from DBMS_METADATA"
    );
    assert!(b.missing.is_empty(), "{:?}", b.missing);
    for sql in [
        "DROP TRIGGER it_kb_bi",
        "DROP PACKAGE it_kb_pkg",
        "DROP SEQUENCE it_kb_seq",
    ] {
        drop_quietly(&db, sql);
    }
    for t in ["IT_KB_CHILD", "IT_KB_PARENT"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
}

#[test]
#[ignore]
fn plscope_gives_exact_calls_and_tables_for_packages_compiled_with_it() {
    let (db, p) = connect();
    let owner = p.user.to_uppercase();
    drop_quietly(&db, "DROP PACKAGE it_ps_a");
    drop_quietly(&db, "DROP PACKAGE it_ps_b");
    drop_quietly(&db, "DROP TABLE it_ps_t PURGE");
    drop_quietly(&db, "DROP TABLE it_ps_u PURGE");
    exec(&db, "CREATE TABLE it_ps_t (id NUMBER)");
    exec(&db, "CREATE TABLE it_ps_u (id NUMBER)");
    exec(
        &db,
        "ALTER SESSION SET plscope_settings = 'IDENTIFIERS:ALL, STATEMENTS:ALL'",
    );
    exec(
        &db,
        "CREATE OR REPLACE PACKAGE it_ps_b AS PROCEDURE go; END it_ps_b;",
    );
    exec(
        &db,
        "CREATE OR REPLACE PACKAGE BODY it_ps_b AS PROCEDURE go IS BEGIN NULL; END; END it_ps_b;",
    );
    exec(
        &db,
        "CREATE OR REPLACE PACKAGE it_ps_a AS PROCEDURE run_it; END it_ps_a;",
    );
    exec(
        &db,
        "CREATE OR REPLACE PACKAGE BODY it_ps_a AS\n  PROCEDURE run_it IS\n    n NUMBER;\n  BEGIN\n    SELECT COUNT(*) INTO n FROM it_ps_t, it_ps_u;\n    INSERT INTO it_ps_u SELECT id FROM it_ps_t;\n    it_ps_b.go;\n  END;\nEND it_ps_a;",
    );
    let a = ora_core::packages::analyze_schema(&db, &owner).unwrap();
    assert!(a.scope_error.is_none(), "{:?}", a.scope_error);
    let pa = a
        .packages
        .iter()
        .find(|x| x.name == "IT_PS_A")
        .expect("IT_PS_A");
    assert_eq!(pa.analysis, "plscope", "compiled with PL/Scope");
    let m = pa.members.iter().find(|m| m.name == "RUN_IT").unwrap();
    assert_eq!(m.source, "plscope");
    assert!(
        m.calls
            .iter()
            .any(|c| c.package == "IT_PS_B" && c.member == "GO"),
        "{:?}",
        m.calls
    );
    assert!(
        m.reads.contains(&"IT_PS_T".to_string()) && m.reads.contains(&"IT_PS_U".to_string()),
        "second table of a FROM list: {:?}",
        m.reads
    );
    assert!(m.writes.contains(&"IT_PS_U".to_string()));
    assert!(
        m.table_lines.iter().any(|t| t.line == 6),
        "line numbers: {:?}",
        m.table_lines
    );
    assert_eq!(pa.unattributed, 0);
    drop_quietly(&db, "DROP PACKAGE it_ps_a");
    drop_quietly(&db, "DROP PACKAGE it_ps_b");
    drop_quietly(&db, "DROP TABLE it_ps_t PURGE");
    drop_quietly(&db, "DROP TABLE it_ps_u PURGE");
}

#[test]
#[ignore]
fn data_compare_streams_two_tables_by_key_and_schema_compare_sees_a_new_table() {
    use ora_core::datadiff::{compare, CursorStream, DataDiffOptions};
    use ora_core::db::Outcome;
    use std::sync::atomic::AtomicBool;
    let (db, p) = connect();
    let owner = p.user.to_uppercase();
    for t in ["IT_CMP_A", "IT_CMP_B", "IT_CMP_NEW"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
    exec(&db, "CREATE TABLE it_cmp_a (id NUMBER PRIMARY KEY, name VARCHAR2(20), amount NUMBER(10,2), hired DATE)");
    exec(&db, "CREATE TABLE it_cmp_b (id NUMBER PRIMARY KEY, name VARCHAR2(20), amount NUMBER(10,2), hired DATE)");
    exec(
        &db,
        "INSERT INTO it_cmp_a VALUES (1, 'Ann', 100, DATE '2021-05-01')",
    );
    exec(
        &db,
        "INSERT INTO it_cmp_a VALUES (2, 'Bob', 200.5, DATE '2021-06-01')",
    );
    exec(&db, "INSERT INTO it_cmp_a VALUES (3, 'Cy', NULL, NULL)");
    exec(
        &db,
        "INSERT INTO it_cmp_b VALUES (1, 'Ann', 100.00, DATE '2021-05-01')",
    );
    exec(
        &db,
        "INSERT INTO it_cmp_b VALUES (2, 'Bob', 201, DATE '2021-06-01')",
    );
    exec(&db, "INSERT INTO it_cmp_b VALUES (4, 'Dee', 1, NULL)");
    exec(
        &db,
        "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' NLS_NUMERIC_CHARACTERS = '.,'",
    );
    let open = |t: &str| match db.run(&format!("SELECT * FROM {t}"), &[], 100).unwrap() {
        Outcome::Rows(c) => CursorStream::new(c),
        _ => panic!("no rows"),
    };
    let (mut a, mut b) = (open("it_cmp_a"), open("it_cmp_b"));
    let o = DataDiffOptions {
        keys: vec!["ID".into()],
        ..Default::default()
    };
    let r = compare(
        &mut a,
        &mut b,
        &o,
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(
        (r.same, r.changed, r.only_a, r.only_b),
        (1, 1, 1, 1),
        "{r:?}"
    );
    assert_eq!(r.changed_samples[0].diffs[0].column, "AMOUNT");

    let before = ora_core::knowledge::collect_schema(&db, &owner, 0).unwrap();
    exec(&db, "CREATE TABLE it_cmp_new (id NUMBER)");
    let after = ora_core::knowledge::collect_schema(&db, &owner, 0).unwrap();
    let d = ora_core::schemadiff::compare_schemas(&after, &before, None, None);
    assert!(
        d.tables_only_a.contains(&"IT_CMP_NEW".to_string()),
        "{:?}",
        d.tables_only_a
    );
    assert!(
        d.script.contains("CREATE TABLE") && d.script.contains("IT_CMP_NEW"),
        "{}",
        d.script
    );
    for t in ["IT_CMP_A", "IT_CMP_B", "IT_CMP_NEW"] {
        drop_quietly(&db, &format!("DROP TABLE {t} PURGE"));
    }
}

/// Other ways to connect. Set what applies, for example:
///   ORA_TNS_ADMIN=/path/to/network/admin ORA_TNS_ALIAS=PROD ORA_USER=… ORA_PASSWORD=…
///   ORA_CONNECT_STRING='host:1521/svc?retry_count=3' …
///   ORA_EXTERNAL=1 ORA_TNS_ALIAS=…   (wallet / OS login, no user or password)
#[test]
#[ignore]
fn other_connection_methods_reach_the_database_and_report_what_they_reached() {
    use ora_core::profile::{AuthKind, ConnKind};
    let mut p = ConnectionProfile::easy("it", "", 1521, "", &env("ORA_USER", ""));
    if let Ok(cs) = std::env::var("ORA_CONNECT_STRING") {
        p.kind = ConnKind::ConnectString;
        p.connect_string = cs;
    } else {
        p.kind = ConnKind::Tns;
        p.tns_alias =
            std::env::var("ORA_TNS_ALIAS").expect("set ORA_TNS_ALIAS or ORA_CONNECT_STRING");
        p.tns_admin = env("ORA_TNS_ADMIN", "");
    }
    if std::env::var("ORA_EXTERNAL").is_ok() {
        p.auth = AuthKind::External;
        p.user = String::new();
    }
    let db = OracleDb::connect(&p, &env("ORA_PASSWORD", "")).unwrap_or_else(|e| panic!("{e}"));
    let d = ora_core::conninfo::details(&db).unwrap();
    println!(
        "{d:#?}\nOracle client {:?}",
        ora_core::oracle_backend::client_version()
    );
    assert!(d.major.is_some() && !d.session_user.is_empty());
}

#[test]
#[ignore]
fn explain_plan_reads_the_plan_and_cleans_up() {
    let (db, _) = connect();
    let p = ora_core::plan::explain(&db, "SELECT * FROM dual WHERE dummy = 'X'").expect("explain");
    assert!(
        !p.nodes.is_empty() && p.nodes[0].operation.contains("SELECT"),
        "{:?}",
        p.nodes
    );
    assert!(!p.text.is_empty());
    // The statement was not run and no plan rows are left behind for this statement id.
    let (_, rows) = collect(
        &db,
        "SELECT COUNT(*) FROM plan_table WHERE statement_id LIKE 'ORAIDE%'",
        &[],
    )
    .unwrap();
    assert!(format!("{:?}", rows[0][0]).contains('0'), "{:?}", rows[0]);
}
