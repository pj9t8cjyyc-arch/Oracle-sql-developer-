//! In-memory catalog snapshot used by autocomplete and the search indexer.
//! Loaded with a handful of bulk dictionary queries per schema.

use std::collections::{BTreeMap, HashMap};

use crate::db::{collect, Database, Row};
use crate::schema::display_type;
use crate::Result;

const SQL_TABLES: &str = "SELECT /*meta:tables*/ table_name, table_type, comments FROM all_tab_comments WHERE owner = :1 AND table_name NOT LIKE 'BIN$%' ORDER BY table_name";

const SQL_COLUMNS: &str = "SELECT /*meta:cols*/ c.table_name, c.column_name, c.data_type, c.data_length, c.char_length, c.char_used, \
       c.data_precision, c.data_scale, cc.comments \
     FROM all_tab_columns c LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name AND cc.column_name = c.column_name \
     WHERE c.owner = :1 AND c.table_name NOT LIKE 'BIN$%' ORDER BY c.table_name, c.column_id";

const SQL_PK: &str = "SELECT /*meta:pk*/ k.table_name, cc.column_name FROM all_constraints k \
     JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
     WHERE k.owner = :1 AND k.constraint_type = 'P'";

const SQL_FK: &str = "SELECT /*meta:fk*/ k.constraint_name, k.table_name, cc.column_name, r.owner, r.table_name, rc.column_name \
     FROM all_constraints k \
     JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
     JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
     JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name AND rc.position = cc.position \
     WHERE k.owner = :1 AND k.constraint_type = 'R' ORDER BY k.constraint_name, cc.position";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColMeta {
    pub name: String,
    pub data_type: String,
    pub comment: Option<String>,
    pub pk: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMeta {
    pub name: String,
    pub is_view: bool,
    pub comment: Option<String>,
    pub columns: Vec<ColMeta>,
}

/// One foreign key (possibly composite): `child_cols[i]` references `parent_cols[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FkEdge {
    pub name: String,
    pub child_table: String,
    pub child_cols: Vec<String>,
    pub parent_owner: String,
    pub parent_table: String,
    pub parent_cols: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaMeta {
    pub owner: String,
    pub tables: BTreeMap<String, TableMeta>,
    pub fks: Vec<FkEdge>,
}

#[derive(Debug, Default)]
pub struct MetaCache {
    pub schemas: HashMap<String, SchemaMeta>,
    /// Every schema name known to the database (for `schema.` completion).
    pub owners: Vec<String>,
}

impl MetaCache {
    pub fn insert(&mut self, s: SchemaMeta) {
        self.schemas.insert(s.owner.clone(), s);
    }
    pub fn table(&self, owner: &str, name: &str) -> Option<&TableMeta> {
        self.schemas.get(owner)?.tables.get(name)
    }
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.is_empty())
}

pub fn load_schema(db: &dyn Database, owner: &str) -> Result<SchemaMeta> {
    let b = [owner.to_string()];
    let mut tables: BTreeMap<String, TableMeta> = BTreeMap::new();
    for r in &collect(db, SQL_TABLES, &b)?.1 {
        let name = s(r, 0);
        tables.insert(
            name.clone(),
            TableMeta {
                name,
                is_view: s(r, 1) == "VIEW",
                comment: opt(r, 2),
                columns: vec![],
            },
        );
    }
    for r in &collect(db, SQL_COLUMNS, &b)?.1 {
        if let Some(t) = tables.get_mut(&s(r, 0)) {
            t.columns.push(ColMeta {
                name: s(r, 1),
                data_type: display_type(&s(r, 2), &s(r, 3), &s(r, 4), &s(r, 5), &s(r, 6), &s(r, 7)),
                comment: opt(r, 8),
                pk: false,
            });
        }
    }
    for r in &collect(db, SQL_PK, &b)?.1 {
        if let Some(c) = tables
            .get_mut(&s(r, 0))
            .and_then(|t| t.columns.iter_mut().find(|c| c.name == s(r, 1)))
        {
            c.pk = true;
        }
    }
    let mut fks: Vec<FkEdge> = Vec::new();
    for r in &collect(db, SQL_FK, &b)?.1 {
        let name = s(r, 0);
        if fks.last().map(|f| f.name != name).unwrap_or(true) {
            fks.push(FkEdge {
                name,
                child_table: s(r, 1),
                child_cols: vec![],
                parent_owner: s(r, 3),
                parent_table: s(r, 4),
                parent_cols: vec![],
            });
        }
        let f = fks.last_mut().unwrap();
        f.child_cols.push(s(r, 2));
        f.parent_cols.push(s(r, 5));
    }
    Ok(SchemaMeta {
        owner: owner.to_string(),
        tables,
        fks,
    })
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn col(name: &str, ty: &str, pk: bool) -> ColMeta {
        ColMeta {
            name: name.into(),
            data_type: ty.into(),
            comment: None,
            pk,
        }
    }

    /// HR: DEPT(ID pk, NAME), EMP(ID pk, NAME, DEPT_ID -> DEPT.ID, MGR_ID -> EMP.ID), VW_EMP view.
    pub fn hr() -> MetaCache {
        let mut tables = BTreeMap::new();
        let mut add = |name: &str, view: bool, cols: Vec<ColMeta>| {
            tables.insert(
                name.to_string(),
                TableMeta {
                    name: name.into(),
                    is_view: view,
                    comment: None,
                    columns: cols,
                },
            );
        };
        add(
            "DEPT",
            false,
            vec![
                col("ID", "NUMBER(6)", true),
                col("NAME", "VARCHAR2(30)", false),
            ],
        );
        add(
            "EMP",
            false,
            vec![
                col("ID", "NUMBER(6)", true),
                col("NAME", "VARCHAR2(50)", false),
                col("DEPT_ID", "NUMBER(6)", false),
                col("MGR_ID", "NUMBER(6)", false),
            ],
        );
        add("VW_EMP", true, vec![col("ID", "NUMBER", false)]);
        let fk = |n: &str, ct: &str, cc: &str, pt: &str| FkEdge {
            name: n.into(),
            child_table: ct.into(),
            child_cols: vec![cc.into()],
            parent_owner: "HR".into(),
            parent_table: pt.into(),
            parent_cols: vec!["ID".into()],
        };
        let mut c = MetaCache {
            owners: vec!["HR".into(), "SALES".into()],
            ..Default::default()
        };
        c.insert(SchemaMeta {
            owner: "HR".into(),
            tables,
            fks: vec![
                fk("EMP_DEPT_FK", "EMP", "DEPT_ID", "DEPT"),
                fk("EMP_MGR_FK", "EMP", "MGR_ID", "EMP"),
            ],
        });
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    #[test]
    fn loads_tables_columns_keys_and_composite_fks() {
        let n = None;
        let db = FakeDb::new()
            .with_table(
                "meta:tables",
                FakeTable::new(
                    &["n", "t", "c"],
                    vec![
                        vec![Some("EMP"), Some("TABLE"), Some("People")],
                        vec![Some("V"), Some("VIEW"), n],
                    ],
                ),
            )
            .with_table(
                "meta:cols",
                FakeTable::new(
                    &["t", "c", "dt", "dl", "cl", "cu", "p", "s", "cm"],
                    vec![
                        vec![
                            Some("EMP"),
                            Some("ID"),
                            Some("NUMBER"),
                            Some("22"),
                            n,
                            n,
                            Some("6"),
                            Some("0"),
                            Some("key"),
                        ],
                        vec![
                            Some("EMP"),
                            Some("NAME"),
                            Some("VARCHAR2"),
                            Some("50"),
                            Some("50"),
                            Some("B"),
                            n,
                            n,
                            n,
                        ],
                        vec![
                            Some("GONE"),
                            Some("X"),
                            Some("DATE"),
                            Some("7"),
                            n,
                            n,
                            n,
                            n,
                            n,
                        ],
                    ],
                ),
            )
            .with_table(
                "meta:pk",
                FakeTable::new(&["t", "c"], vec![vec![Some("EMP"), Some("ID")]]),
            )
            .with_table(
                "meta:fk",
                FakeTable::new(
                    &["cn", "t", "c", "ro", "rt", "rc"],
                    vec![
                        vec![
                            Some("F"),
                            Some("EMP"),
                            Some("A"),
                            Some("HR"),
                            Some("P"),
                            Some("X"),
                        ],
                        vec![
                            Some("F"),
                            Some("EMP"),
                            Some("B"),
                            Some("HR"),
                            Some("P"),
                            Some("Y"),
                        ],
                    ],
                ),
            );
        let m = load_schema(&db, "HR").unwrap();
        let emp = &m.tables["EMP"];
        assert_eq!(emp.comment.as_deref(), Some("People"));
        assert!(m.tables["V"].is_view);
        assert_eq!(
            emp.columns
                .iter()
                .map(|c| (c.name.as_str(), c.data_type.as_str(), c.pk))
                .collect::<Vec<_>>(),
            vec![("ID", "NUMBER(6)", true), ("NAME", "VARCHAR2(50)", false)]
        );
        assert!(
            !m.tables.contains_key("GONE"),
            "columns of unknown tables are ignored"
        );
        assert_eq!(m.fks.len(), 1);
        assert_eq!(
            (m.fks[0].child_cols.clone(), m.fks[0].parent_cols.clone()),
            (
                vec!["A".to_string(), "B".to_string()],
                vec!["X".to_string(), "Y".to_string()]
            )
        );
    }
}
