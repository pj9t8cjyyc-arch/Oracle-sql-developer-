//! Schema browser queries. Every user-controlled value is a bind variable.

use serde::Serialize;

use crate::db::{collect, Database, Row};
use crate::Result;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DbObject {
    pub name: String,
    pub object_type: String,
    pub valid: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// Display type, e.g. `VARCHAR2(30 CHAR)` or `NUMBER(10,2)`.
    pub data_type: String,
    pub nullable: bool,
}

pub const SQL_SCHEMAS: &str =
    "SELECT username FROM all_users WHERE oracle_maintained = 'N' ORDER BY username";

pub const SQL_OBJECTS: &str = "SELECT object_name, object_type, status FROM all_objects \
     WHERE owner = :1 AND generated = 'N' AND object_name NOT LIKE 'BIN$%' \
     AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','PROCEDURE','FUNCTION','PACKAGE','TRIGGER','SYNONYM','TYPE') \
     ORDER BY object_type, object_name";

pub const SQL_COLUMNS: &str = "SELECT column_name, data_type, data_length, char_length, char_used, data_precision, data_scale, nullable \
     FROM all_tab_columns WHERE owner = :1 AND table_name = :2 ORDER BY column_id";

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

pub fn schemas(db: &dyn Database) -> Result<Vec<String>> {
    Ok(collect(db, SQL_SCHEMAS, &[])?
        .1
        .iter()
        .map(|r| s(r, 0))
        .collect())
}

pub fn objects(db: &dyn Database, owner: &str) -> Result<Vec<DbObject>> {
    let (_, rows) = collect(db, SQL_OBJECTS, &[owner.to_string()])?;
    Ok(rows
        .iter()
        .map(|r| DbObject {
            name: s(r, 0),
            object_type: s(r, 1),
            valid: s(r, 2) == "VALID",
        })
        .collect())
}

pub fn columns(db: &dyn Database, owner: &str, table: &str) -> Result<Vec<TableColumn>> {
    let (_, rows) = collect(db, SQL_COLUMNS, &[owner.to_string(), table.to_string()])?;
    Ok(rows
        .iter()
        .map(|r| TableColumn {
            name: s(r, 0),
            data_type: display_type(&s(r, 1), &s(r, 2), &s(r, 3), &s(r, 4), &s(r, 5), &s(r, 6)),
            nullable: s(r, 7) == "Y",
        })
        .collect())
}

pub(crate) fn display_type(
    t: &str,
    data_len: &str,
    char_len: &str,
    char_used: &str,
    prec: &str,
    scale: &str,
) -> String {
    match t {
        "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => {
            let unit = if char_used == "C" { " CHAR" } else { "" };
            let len = if char_used == "C" { char_len } else { data_len };
            format!("{t}({len}{unit})")
        }
        "RAW" => format!("RAW({data_len})"),
        "NUMBER" => match (prec.is_empty(), scale.is_empty()) {
            (true, _) => "NUMBER".into(),
            (false, true) => format!("NUMBER({prec})"),
            (false, false) if scale == "0" => format!("NUMBER({prec})"),
            (false, false) => format!("NUMBER({prec},{scale})"),
        },
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    #[test]
    fn owner_is_bound_never_concatenated() {
        let db = FakeDb::new().with_table(
            "from all_objects",
            FakeTable::new(
                &["N", "T", "S"],
                vec![
                    vec![Some("EMP"), Some("TABLE"), Some("VALID")],
                    vec![Some("V1"), Some("VIEW"), Some("INVALID")],
                ],
            ),
        );
        let evil = "HR' OR '1'='1";
        let objs = objects(&db, evil).unwrap();
        assert_eq!(
            objs,
            vec![
                DbObject {
                    name: "EMP".into(),
                    object_type: "TABLE".into(),
                    valid: true
                },
                DbObject {
                    name: "V1".into(),
                    object_type: "VIEW".into(),
                    valid: false
                },
            ]
        );
        let log = db.log.lock().unwrap();
        assert!(!log.calls[0].0.contains("OR '1'"));
        assert_eq!(log.calls[0].1, vec![evil.to_string()]);
    }

    #[test]
    fn type_display() {
        assert_eq!(
            display_type("VARCHAR2", "120", "30", "C", "", ""),
            "VARCHAR2(30 CHAR)"
        );
        assert_eq!(
            display_type("VARCHAR2", "30", "30", "B", "", ""),
            "VARCHAR2(30)"
        );
        assert_eq!(
            display_type("NUMBER", "22", "", "", "10", "2"),
            "NUMBER(10,2)"
        );
        assert_eq!(
            display_type("NUMBER", "22", "", "", "10", "0"),
            "NUMBER(10)"
        );
        assert_eq!(display_type("NUMBER", "22", "", "", "", ""), "NUMBER");
        assert_eq!(display_type("DATE", "7", "", "", "", ""), "DATE");
    }

    #[test]
    fn null_cells_do_not_panic() {
        let db = FakeDb::new().with_table(
            "from all_users",
            FakeTable::new(&["U"], vec![vec![None], vec![Some("HR")]]),
        );
        assert_eq!(
            schemas(&db).unwrap(),
            vec!["".to_string(), "HR".to_string()]
        );
    }
}
