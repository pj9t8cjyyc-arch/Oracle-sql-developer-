//! Data-dictionary browsing: what exists in a schema, how it is defined, and
//! its source/DDL. Every user-controlled value is a bind variable; object
//! kinds are checked against allow-lists before they reach SQL or
//! `DBMS_METADATA`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::comments::quote_ident;
use crate::db::{collect, Database, Row};
use crate::{Error, Result};

/// Object kinds shown in the explorer, in display order: (key, label).
pub const KINDS: &[(&str, &str)] = &[
    ("TABLE", "Tables"),
    ("VIEW", "Views"),
    ("MATERIALIZED VIEW", "Materialized views"),
    ("INDEX", "Indexes"),
    ("SEQUENCE", "Sequences"),
    ("TRIGGER", "Triggers"),
    ("PROCEDURE", "Procedures"),
    ("FUNCTION", "Functions"),
    ("PACKAGE", "Packages"),
    ("TYPE", "Types"),
    ("SYNONYM", "Synonyms"),
];

fn check_kind(kind: &str) -> Result<&'static str> {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(k, _)| *k)
        .ok_or_else(|| Error::Invalid(format!("unknown object kind {kind:?}")))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ObjectEntry {
    pub name: String,
    /// VALID / INVALID / ENABLED / DISABLED / UNUSABLE …
    pub status: String,
    pub ok: bool,
    /// One-line description, e.g. `BEFORE EACH ROW · INSERT · on EMP`.
    pub detail: Option<String>,
    /// Table a trigger or index belongs to.
    pub parent: Option<String>,
}

const SQL_COUNTS: &str = "SELECT /*cat:counts*/ object_type, COUNT(*) FROM all_objects \
     WHERE owner = :1 AND object_name NOT LIKE 'BIN$%' AND generated = 'N' \
       AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TYPE','SYNONYM') \
     GROUP BY object_type \
     UNION ALL SELECT 'INDEX', COUNT(*) FROM all_indexes WHERE owner = :1 \
     UNION ALL SELECT 'TRIGGER', COUNT(*) FROM all_triggers WHERE owner = :1";

const SQL_OBJECTS: &str = "SELECT /*cat:objects*/ object_name, status FROM all_objects \
     WHERE owner = :1 AND object_type = :2 AND generated = 'N' AND object_name NOT LIKE 'BIN$%' ORDER BY object_name";
const SQL_INDEXES: &str = "SELECT /*cat:indexes*/ index_name, status, table_name, uniqueness, index_type FROM all_indexes WHERE owner = :1 ORDER BY index_name";
const SQL_TRIGGERS: &str = "SELECT /*cat:triggers*/ trigger_name, status, table_name, triggering_event, trigger_type FROM all_triggers WHERE owner = :1 ORDER BY trigger_name";
const SQL_SEQUENCES: &str = "SELECT /*cat:sequences*/ sequence_name, increment_by, last_number FROM all_sequences \
     WHERE sequence_owner = :1 AND sequence_name NOT LIKE 'ISEQ$$\\_%' ESCAPE '\\' ORDER BY sequence_name";
const SQL_SYNONYMS: &str = "SELECT /*cat:synonyms*/ synonym_name, table_owner, table_name, db_link FROM all_synonyms WHERE owner = :1 ORDER BY synonym_name";

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.is_empty())
}
fn is_ok(status: &str) -> bool {
    matches!(status, "" | "VALID" | "ENABLED" | "N/A")
}

/// Number of objects per kind. Kinds with none are omitted.
pub fn object_counts(db: &dyn Database, owner: &str) -> Result<BTreeMap<String, u64>> {
    let (_, rows) = collect(db, SQL_COUNTS, &[owner.to_string()])?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            s(r, 1)
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| (s(r, 0), n))
        })
        .collect())
}

pub fn list_objects(db: &dyn Database, owner: &str, kind: &str) -> Result<Vec<ObjectEntry>> {
    let kind = check_kind(kind)?;
    let o = [owner.to_string()];
    let entry = |name: String, status: String, detail: Option<String>, parent: Option<String>| {
        let ok = is_ok(&status);
        ObjectEntry {
            name,
            status,
            ok,
            detail,
            parent,
        }
    };
    Ok(match kind {
        "INDEX" => collect(db, SQL_INDEXES, &o)?
            .1
            .iter()
            .map(|r| {
                let detail = format!("{} · {} · on {}", s(r, 3), s(r, 4), s(r, 2));
                entry(s(r, 0), s(r, 1), Some(detail), opt(r, 2))
            })
            .collect(),
        "TRIGGER" => collect(db, SQL_TRIGGERS, &o)?
            .1
            .iter()
            .map(|r| {
                let on = opt(r, 2).map(|t| format!(" · on {t}")).unwrap_or_default();
                entry(
                    s(r, 0),
                    s(r, 1),
                    Some(format!("{} · {}{}", s(r, 4), s(r, 3), on)),
                    opt(r, 2),
                )
            })
            .collect(),
        "SEQUENCE" => collect(db, SQL_SEQUENCES, &o)?
            .1
            .iter()
            .map(|r| {
                entry(
                    s(r, 0),
                    "VALID".into(),
                    Some(format!("+{} · last {}", s(r, 1), s(r, 2))),
                    None,
                )
            })
            .collect(),
        "SYNONYM" => collect(db, SQL_SYNONYMS, &o)?
            .1
            .iter()
            .map(|r| {
                let link = opt(r, 3).map(|l| format!("@{l}")).unwrap_or_default();
                entry(
                    s(r, 0),
                    "VALID".into(),
                    Some(format!("→ {}.{}{}", s(r, 1), s(r, 2), link)),
                    None,
                )
            })
            .collect(),
        k => collect(db, SQL_OBJECTS, &[owner.to_string(), k.to_string()])?
            .1
            .iter()
            .map(|r| entry(s(r, 0), s(r, 1), None, None))
            .collect(),
    })
}

const SOURCE_TYPES: &[&str] = &[
    "PROCEDURE",
    "FUNCTION",
    "PACKAGE",
    "PACKAGE BODY",
    "TRIGGER",
    "TYPE",
    "TYPE BODY",
];

/// PL/SQL source text (empty for wrapped code).
pub fn object_source(db: &dyn Database, owner: &str, name: &str, kind: &str) -> Result<String> {
    if !SOURCE_TYPES.contains(&kind) {
        return Err(Error::Invalid(format!(
            "no PL/SQL source for kind {kind:?}"
        )));
    }
    let (_, rows) = collect(db, "SELECT /*cat:source*/ text FROM all_source WHERE owner = :1 AND name = :2 AND type = :3 ORDER BY line", &[owner.to_string(), name.to_string(), kind.to_string()])?;
    Ok(rows.iter().map(|r| s(r, 0)).collect())
}

/// `DBMS_METADATA` object type for a kind shown in the explorer.
fn ddl_type(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "TABLE" => "TABLE",
        "VIEW" => "VIEW",
        "MATERIALIZED VIEW" => "MATERIALIZED_VIEW",
        "INDEX" => "INDEX",
        "SEQUENCE" => "SEQUENCE",
        "TRIGGER" => "TRIGGER",
        "PROCEDURE" => "PROCEDURE",
        "FUNCTION" => "FUNCTION",
        "PACKAGE" => "PACKAGE",
        "PACKAGE BODY" => "PACKAGE_BODY",
        "TYPE" => "TYPE",
        "TYPE BODY" => "TYPE_BODY",
        "SYNONYM" => "SYNONYM",
        _ => return None,
    })
}

/// Full DDL from `DBMS_METADATA.GET_DDL`. Needs SELECT_CATALOG_ROLE or
/// ownership; the error is passed through when it is missing.
pub fn object_ddl(db: &dyn Database, owner: &str, name: &str, kind: &str) -> Result<String> {
    let t = ddl_type(kind).ok_or_else(|| Error::Invalid(format!("no DDL for kind {kind:?}")))?;
    let (_, rows) = collect(
        db,
        "SELECT /*cat:ddl*/ DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual",
        &[t.to_string(), name.to_string(), owner.to_string()],
    )?;
    Ok(rows
        .first()
        .map(|r| s(r, 0).trim().to_string())
        .unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SequenceInfo {
    pub owner: String,
    pub name: String,
    pub min: String,
    pub max: String,
    pub increment: String,
    pub cycle: bool,
    pub ordered: bool,
    pub cache: String,
    /// Next value the database will hand out (not necessarily the last one used when cached).
    pub last_number: String,
}

pub fn sequence_info(db: &dyn Database, owner: &str, name: &str) -> Result<Option<SequenceInfo>> {
    let (_, rows) = collect(
        db,
        "SELECT /*cat:seq*/ min_value, max_value, increment_by, cycle_flag, order_flag, cache_size, last_number FROM all_sequences WHERE sequence_owner = :1 AND sequence_name = :2",
        &[owner.to_string(), name.to_string()],
    )?;
    Ok(rows.first().map(|r| SequenceInfo {
        owner: owner.to_string(),
        name: name.to_string(),
        min: s(r, 0),
        max: s(r, 1),
        increment: s(r, 2),
        cycle: s(r, 3) == "Y",
        ordered: s(r, 4) == "Y",
        cache: s(r, 5),
        last_number: s(r, 6),
    }))
}

/// `SELECT * … WHERE ROWNUM <= n` with quoted identifiers (works on every Oracle version).
pub fn preview_sql(owner: &str, table: &str, limit: usize) -> Result<String> {
    Ok(format!(
        "SELECT * FROM {}.{} WHERE ROWNUM <= {}",
        quote_ident(owner)?,
        quote_ident(table)?,
        limit.clamp(1, 1000)
    ))
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Dep {
    pub owner: String,
    pub name: String,
    pub object_type: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct Dependencies {
    /// Objects this one references (SYS-owned built-ins are left out).
    pub uses: Vec<Dep>,
    /// Objects that reference this one.
    pub used_by: Vec<Dep>,
}

/// What an object depends on and what depends on it, from `ALL_DEPENDENCIES`.
pub fn dependencies(db: &dyn Database, owner: &str, name: &str) -> Result<Dependencies> {
    let b = [owner.to_string(), name.to_string()];
    let dep = |r: &Row| Dep {
        owner: s(r, 0),
        name: s(r, 1),
        object_type: s(r, 2),
    };
    let uses = collect(
        db,
        "SELECT /*cat:uses*/ referenced_owner, referenced_name, referenced_type FROM all_dependencies \
         WHERE owner = :1 AND name = :2 AND referenced_owner <> 'SYS' ORDER BY referenced_type, referenced_name",
        &b,
    )?;
    let used_by = collect(
        db,
        "SELECT /*cat:usedby*/ owner, name, type FROM all_dependencies WHERE referenced_owner = :1 AND referenced_name = :2 ORDER BY type, name",
        &b,
    )?;
    Ok(Dependencies {
        uses: uses.1.iter().map(dep).collect(),
        used_by: used_by.1.iter().map(dep).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn ft(cols: &[&str], rows: Vec<Vec<Option<&str>>>) -> FakeTable {
        FakeTable::new(cols, rows)
    }

    #[test]
    fn counts_skip_empty_kinds_and_are_keyed_by_kind() {
        let db = FakeDb::new().with_table(
            "cat:counts",
            ft(
                &["t", "n"],
                vec![
                    vec![Some("TABLE"), Some("12")],
                    vec![Some("INDEX"), Some("30")],
                    vec![Some("TRIGGER"), Some("0")],
                ],
            ),
        );
        let c = object_counts(&db, "HR").unwrap();
        assert_eq!(c.get("TABLE"), Some(&12));
        assert_eq!(c.get("INDEX"), Some(&30));
        assert!(!c.contains_key("TRIGGER"), "zero counts are omitted");
        assert_eq!(db.log.lock().unwrap().calls[0].1, vec!["HR".to_string()]);
    }

    #[test]
    fn indexes_and_triggers_carry_their_table() {
        let db = FakeDb::new()
            .with_table(
                "cat:indexes",
                ft(
                    &["i", "s", "t", "u", "ty"],
                    vec![
                        vec![
                            Some("EMP_PK"),
                            Some("VALID"),
                            Some("EMP"),
                            Some("UNIQUE"),
                            Some("NORMAL"),
                        ],
                        vec![
                            Some("EMP_X"),
                            Some("UNUSABLE"),
                            Some("EMP"),
                            Some("NONUNIQUE"),
                            Some("BITMAP"),
                        ],
                    ],
                ),
            )
            .with_table(
                "cat:triggers",
                ft(
                    &["n", "s", "t", "e", "ty"],
                    vec![
                        vec![
                            Some("EMP_BI"),
                            Some("ENABLED"),
                            Some("EMP"),
                            Some("INSERT OR UPDATE"),
                            Some("BEFORE EACH ROW"),
                        ],
                        vec![
                            Some("LOGON_TRG"),
                            Some("DISABLED"),
                            None,
                            Some("LOGON"),
                            Some("AFTER EVENT"),
                        ],
                    ],
                ),
            );
        let ix = list_objects(&db, "HR", "INDEX").unwrap();
        assert_eq!(
            (ix[0].parent.as_deref(), ix[0].ok, ix[0].detail.as_deref()),
            (Some("EMP"), true, Some("UNIQUE · NORMAL · on EMP"))
        );
        assert!(!ix[1].ok, "UNUSABLE index is flagged");
        let tr = list_objects(&db, "HR", "TRIGGER").unwrap();
        assert_eq!(
            tr[0].detail.as_deref(),
            Some("BEFORE EACH ROW · INSERT OR UPDATE · on EMP")
        );
        assert!(
            !tr[1].ok && tr[1].parent.is_none(),
            "schema-level trigger has no table; disabled is flagged"
        );
    }

    #[test]
    fn sequences_hide_identity_helpers_in_sql_and_synonyms_show_targets() {
        let db = FakeDb::new()
            .with_table(
                "cat:sequences",
                ft(
                    &["n", "i", "l"],
                    vec![vec![Some("EMP_SEQ"), Some("1"), Some("1050")]],
                ),
            )
            .with_table(
                "cat:synonyms",
                ft(
                    &["n", "o", "t", "l"],
                    vec![
                        vec![Some("E"), Some("HR"), Some("EMP"), None],
                        vec![Some("R"), Some("X"), Some("T"), Some("REMOTE")],
                    ],
                ),
            );
        let s = list_objects(&db, "HR", "SEQUENCE").unwrap();
        assert_eq!(s[0].detail.as_deref(), Some("+1 · last 1050"));
        assert!(db.log.lock().unwrap().calls[0]
            .0
            .contains("NOT LIKE 'ISEQ$$"));
        let y = list_objects(&db, "HR", "SYNONYM").unwrap();
        assert_eq!(
            (y[0].detail.as_deref(), y[1].detail.as_deref()),
            (Some("→ HR.EMP"), Some("→ X.T@REMOTE"))
        );
    }

    #[test]
    fn plain_kinds_bind_owner_and_kind() {
        let db = FakeDb::new().with_table(
            "cat:objects",
            ft(&["n", "s"], vec![vec![Some("V1"), Some("INVALID")]]),
        );
        let v = list_objects(&db, "HR", "VIEW").unwrap();
        assert!(!v[0].ok);
        assert_eq!(
            db.log.lock().unwrap().calls[0].1,
            vec!["HR".to_string(), "VIEW".to_string()]
        );
    }

    #[test]
    fn unknown_kinds_are_rejected_before_any_sql_runs() {
        let db = FakeDb::new();
        for bad in ["TABLE; DROP TABLE x", "", "table", "USER"] {
            assert!(
                matches!(list_objects(&db, "HR", bad), Err(Error::Invalid(_))),
                "{bad:?}"
            );
            assert!(object_ddl(&db, "HR", "X", bad).is_err());
            assert!(object_source(&db, "HR", "X", bad).is_err());
        }
        assert!(db.log.lock().unwrap().calls.is_empty());
    }

    #[test]
    fn source_joins_lines_and_ddl_maps_kind_names() {
        let db = FakeDb::new()
            .with_table(
                "cat:source",
                ft(
                    &["t"],
                    vec![
                        vec![Some("procedure p is\n")],
                        vec![Some("begin null; end;")],
                    ],
                ),
            )
            .with_table(
                "cat:ddl",
                ft(
                    &["d"],
                    vec![vec![Some("\n  CREATE TABLE \"HR\".\"EMP\" (...)\n")]],
                ),
            );
        assert_eq!(
            object_source(&db, "HR", "P", "PROCEDURE").unwrap(),
            "procedure p is\nbegin null; end;"
        );
        assert!(
            object_source(&db, "HR", "T", "TABLE").is_err(),
            "tables have no PL/SQL source"
        );
        assert_eq!(
            object_ddl(&db, "HR", "EMP", "TABLE").unwrap(),
            "CREATE TABLE \"HR\".\"EMP\" (...)"
        );
        object_ddl(&db, "HR", "MV", "MATERIALIZED VIEW").unwrap();
        let log = db.log.lock().unwrap();
        assert_eq!(
            log.calls.last().unwrap().1,
            vec![
                "MATERIALIZED_VIEW".to_string(),
                "MV".to_string(),
                "HR".to_string()
            ]
        );
    }

    #[test]
    fn sequence_info_and_missing_sequence() {
        let db = FakeDb::new().with_table(
            "cat:seq",
            ft(
                &["a", "b", "c", "d", "e", "f", "g"],
                vec![vec![
                    Some("1"),
                    Some("9999"),
                    Some("5"),
                    Some("Y"),
                    Some("N"),
                    Some("20"),
                    Some("1050"),
                ]],
            ),
        );
        let i = sequence_info(&db, "HR", "EMP_SEQ").unwrap().unwrap();
        assert_eq!(
            (
                i.increment.as_str(),
                i.cycle,
                i.ordered,
                i.cache.as_str(),
                i.last_number.as_str()
            ),
            ("5", true, false, "20", "1050")
        );
        let none = FakeDb::new().with_table("cat:seq", ft(&["a"], vec![]));
        assert!(sequence_info(&none, "HR", "X").unwrap().is_none());
    }

    #[test]
    fn preview_sql_quotes_identifiers_and_clamps_limit() {
        assert_eq!(
            preview_sql("HR", "EMP", 50).unwrap(),
            r#"SELECT * FROM "HR"."EMP" WHERE ROWNUM <= 50"#
        );
        assert!(preview_sql("HR", "EMP", 0).unwrap().ends_with("<= 1"));
        assert!(preview_sql("HR", "EMP", 10_000_000)
            .unwrap()
            .ends_with("<= 1000"));
        assert!(preview_sql("HR", "EMP\" WHERE 1=1 --", 5).is_err());
    }

    #[test]
    fn dependencies_both_directions_are_bound() {
        let db = FakeDb::new()
            .with_table(
                "cat:uses",
                ft(
                    &["o", "n", "t"],
                    vec![vec![Some("HR"), Some("EMPLOYEES"), Some("TABLE")]],
                ),
            )
            .with_table(
                "cat:usedby",
                ft(
                    &["o", "n", "t"],
                    vec![
                        vec![Some("HR"), Some("EMP_PKG"), Some("PACKAGE BODY")],
                        vec![Some("HR"), Some("EMP_BI"), Some("TRIGGER")],
                    ],
                ),
            );
        let d = dependencies(&db, "HR", "EMPLOYEES_SEQ").unwrap();
        assert_eq!(d.uses[0].name, "EMPLOYEES");
        assert_eq!(
            d.used_by
                .iter()
                .map(|x| x.object_type.as_str())
                .collect::<Vec<_>>(),
            vec!["PACKAGE BODY", "TRIGGER"]
        );
        let log = db.log.lock().unwrap();
        assert!(log
            .calls
            .iter()
            .all(|(_, b)| b == &vec!["HR".to_string(), "EMPLOYEES_SEQ".to_string()]));
        assert!(log.calls[0].0.contains("<> 'SYS'"));
    }
}
