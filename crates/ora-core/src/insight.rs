//! Table/column insight: what keys, indexes, triggers, constraints, comments
//! and statistics apply to each column, assembled from data-dictionary views.
//!
//! Each query carries a `/*insight:tag*/` comment so test doubles can match
//! it; Oracle ignores the comment. All identifiers are bind variables.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::db::{collect, Database, Row};
use crate::schema::display_type;
use crate::Result;

const SQL_TAB_COMMENT: &str = "SELECT /*insight:tabcomment*/ comments FROM all_tab_comments WHERE owner = :1 AND table_name = :2";

const SQL_COLUMNS: &str = "SELECT /*insight:cols*/ c.column_name, c.data_type, c.data_length, c.char_length, c.char_used, \
       c.data_precision, c.data_scale, c.nullable, cc.comments, s.num_distinct, s.num_nulls, s.histogram \
     FROM all_tab_columns c \
     LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name AND cc.column_name = c.column_name \
     LEFT JOIN all_tab_col_statistics s ON s.owner = c.owner AND s.table_name = c.table_name AND s.column_name = c.column_name \
     WHERE c.owner = :1 AND c.table_name = :2 ORDER BY c.column_id";

const SQL_CONSTRAINTS: &str = "SELECT /*insight:cons*/ k.constraint_name, k.constraint_type, cc.column_name, k.status, k.search_condition_vc, \
       r.owner, r.table_name, rc.column_name \
     FROM all_constraints k \
     JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
     LEFT JOIN all_constraints r ON k.constraint_type = 'R' AND r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
     LEFT JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name AND rc.position = cc.position \
     WHERE k.owner = :1 AND k.table_name = :2 AND k.constraint_type IN ('P','U','R','C') \
     ORDER BY k.constraint_name, cc.position";

const SQL_CHILDREN: &str = "SELECT /*insight:children*/ k.owner, k.table_name, k.constraint_name \
     FROM all_constraints k JOIN all_constraints p ON k.r_owner = p.owner AND k.r_constraint_name = p.constraint_name \
     WHERE k.constraint_type = 'R' AND p.owner = :1 AND p.table_name = :2 ORDER BY k.table_name, k.constraint_name";

const SQL_INDEXES: &str = "SELECT /*insight:idx*/ i.index_name, i.uniqueness, ic.column_name, ic.column_position, i.status, i.index_type \
     FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
     WHERE i.table_owner = :1 AND i.table_name = :2 ORDER BY i.index_name, ic.column_position";

const SQL_TRIGGERS: &str = "SELECT /*insight:trg*/ t.trigger_name, t.trigger_type, t.triggering_event, t.status, tc.column_name, tc.column_list, tc.column_usage \
     FROM all_triggers t \
     LEFT JOIN all_trigger_cols tc ON tc.trigger_owner = t.owner AND tc.trigger_name = t.trigger_name \
       AND tc.table_owner = t.table_owner AND tc.table_name = t.table_name \
     WHERE t.table_owner = :1 AND t.table_name = :2 ORDER BY t.trigger_name";

const SQL_DEPS: &str = "SELECT /*insight:deps*/ 'USES' dir, referenced_owner, referenced_name, referenced_type FROM all_dependencies \
       WHERE owner = :1 AND name = :2 AND type IN ('TABLE','VIEW') \
     UNION ALL \
     SELECT 'USED_BY', owner, name, type FROM all_dependencies WHERE referenced_owner = :1 AND referenced_name = :2 \
     ORDER BY 1, 3";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FkRef {
    pub constraint: String,
    pub ref_owner: String,
    pub ref_table: String,
    pub ref_column: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ColumnIndex {
    pub name: String,
    /// 1-based position of the column within the index.
    pub position: u32,
    pub unique: bool,
    /// The column is the first one in the index (usable for range/equality lookups on its own).
    pub leading: bool,
    pub valid: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ColumnTrigger {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub enabled: bool,
    /// `update_of`: listed in UPDATE OF; `referenced`: used in the trigger
    /// body (:NEW/:OLD); `any_update`: trigger fires on any UPDATE.
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ColumnInsight {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub comment: Option<String>,
    pub distinct: Option<u64>,
    pub nulls: Option<u64>,
    pub histogram: Option<String>,
    pub primary_key: bool,
    pub unique: bool,
    pub foreign_keys: Vec<FkRef>,
    pub checks: Vec<String>,
    pub indexes: Vec<ColumnIndex>,
    pub triggers: Vec<ColumnTrigger>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub unique: bool,
    pub valid: bool,
    pub index_type: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TriggerInfo {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub enabled: bool,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChildFk {
    pub owner: String,
    pub table: String,
    pub constraint: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Dependency {
    pub owner: String,
    pub name: String,
    pub object_type: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TableInsight {
    pub owner: String,
    pub table: String,
    pub comment: Option<String>,
    pub columns: Vec<ColumnInsight>,
    pub indexes: Vec<IndexInfo>,
    pub triggers: Vec<TriggerInfo>,
    /// Tables whose foreign keys point at this table.
    pub referenced_by: Vec<ChildFk>,
    pub uses: Vec<Dependency>,
    pub used_by: Vec<Dependency>,
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.is_empty())
}
fn num(r: &Row, i: usize) -> Option<u64> {
    opt(r, i)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v >= 0.0)
        .map(|v| v as u64)
}

pub fn table_insight(db: &dyn Database, owner: &str, table: &str) -> Result<TableInsight> {
    let b2 = [owner.to_string(), table.to_string()];

    let comment = collect(db, SQL_TAB_COMMENT, &b2)?
        .1
        .first()
        .and_then(|r| opt(r, 0));

    let (_, col_rows) = collect(db, SQL_COLUMNS, &b2)?;
    let mut columns: Vec<ColumnInsight> = col_rows
        .iter()
        .map(|r| ColumnInsight {
            name: s(r, 0),
            data_type: display_type(&s(r, 1), &s(r, 2), &s(r, 3), &s(r, 4), &s(r, 5), &s(r, 6)),
            nullable: s(r, 7) == "Y",
            comment: opt(r, 8),
            distinct: num(r, 9),
            nulls: num(r, 10),
            histogram: opt(r, 11).filter(|h| h != "NONE"),
            primary_key: false,
            unique: false,
            foreign_keys: vec![],
            checks: vec![],
            indexes: vec![],
            triggers: vec![],
        })
        .collect();

    // Constraints. Rows are (constraint, type, column, status, condition, r_owner, r_table, r_column).
    let (_, rows) = collect(db, SQL_CONSTRAINTS, &b2)?;
    let mut unique_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &rows {
        let (name, ctype, col) = (s(r, 0), s(r, 1), s(r, 2));
        let Some(c) = columns.iter_mut().find(|c| c.name == col) else {
            continue;
        };
        match ctype.as_str() {
            "P" => c.primary_key = true,
            "U" => unique_groups
                .entry(name.clone())
                .or_default()
                .push(col.clone()),
            "R" => c.foreign_keys.push(FkRef {
                constraint: name,
                ref_owner: s(r, 5),
                ref_table: s(r, 6),
                ref_column: s(r, 7),
            }),
            "C" => {
                let cond = s(r, 4);
                // NOT NULL is already shown through `nullable`.
                if !cond.to_uppercase().trim_end().ends_with("IS NOT NULL") && !cond.is_empty() {
                    c.checks.push(cond);
                }
            }
            _ => {}
        }
    }
    // Only a single-column unique constraint makes the column itself unique.
    for cols in unique_groups.values().filter(|v| v.len() == 1) {
        if let Some(c) = columns.iter_mut().find(|c| c.name == cols[0]) {
            c.unique = true;
        }
    }

    // Indexes. Rows are (index, uniqueness, column, position, status, type).
    let (_, rows) = collect(db, SQL_INDEXES, &b2)?;
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for r in &rows {
        let (name, col) = (s(r, 0), s(r, 2));
        let (unique, valid) = (
            s(r, 1) == "UNIQUE",
            matches!(s(r, 4).as_str(), "VALID" | "N/A"),
        );
        if indexes.last().map(|i| i.name != name).unwrap_or(true) {
            indexes.push(IndexInfo {
                name: name.clone(),
                unique,
                valid,
                index_type: s(r, 5),
                columns: vec![],
            });
        }
        indexes.last_mut().unwrap().columns.push(col.clone());
        if let Some(c) = columns.iter_mut().find(|c| c.name == col) {
            c.indexes.push(ColumnIndex {
                name,
                position: s(r, 3).parse().unwrap_or(1),
                unique,
                leading: s(r, 3) == "1",
                valid,
            });
        }
    }

    // Triggers. Rows are (name, type, event, status, column, column_list, usage); one row per column, or one with NULLs.
    let (_, rows) = collect(db, SQL_TRIGGERS, &b2)?;
    let mut triggers: Vec<TriggerInfo> = Vec::new();
    let mut update_of: BTreeMap<String, bool> = BTreeMap::new();
    for r in &rows {
        let name = s(r, 0);
        if triggers.last().map(|t| t.name != name).unwrap_or(true) {
            let ttype = s(r, 1);
            let timing = ttype.replace(" EACH ROW", "").replace(" STATEMENT", "");
            let timing = if ttype.contains("EACH ROW") {
                format!("{timing} (row)")
            } else {
                timing
            };
            triggers.push(TriggerInfo {
                name: name.clone(),
                timing,
                event: s(r, 2),
                enabled: s(r, 3) == "ENABLED",
                columns: vec![],
            });
        }
        if let Some(col) = opt(r, 4) {
            triggers.last_mut().unwrap().columns.push(col);
            if s(r, 5) == "YES" {
                update_of.insert(name, true);
            }
        }
    }
    for (t, rows_for_t) in triggers.iter().map(|t| {
        (
            t,
            rows.iter()
                .filter(|r| s(r, 0) == t.name)
                .collect::<Vec<_>>(),
        )
    }) {
        let restricted = update_of.contains_key(&t.name);
        let fires_on_update = t.event.contains("UPDATE");
        for c in columns.iter_mut() {
            let row = rows_for_t
                .iter()
                .find(|r| opt(r, 4).as_deref() == Some(c.name.as_str()));
            let reason = match row {
                Some(r) if s(r, 5) == "YES" => "update_of",
                Some(_) => "referenced",
                None if fires_on_update && !restricted => "any_update",
                None => continue,
            };
            c.triggers.push(ColumnTrigger {
                name: t.name.clone(),
                timing: t.timing.clone(),
                event: t.event.clone(),
                enabled: t.enabled,
                reason: reason.into(),
            });
        }
    }

    let referenced_by = collect(db, SQL_CHILDREN, &b2)?
        .1
        .iter()
        .map(|r| ChildFk {
            owner: s(r, 0),
            table: s(r, 1),
            constraint: s(r, 2),
        })
        .collect();

    let (mut uses, mut used_by) = (vec![], vec![]);
    for r in &collect(db, SQL_DEPS, &b2)?.1 {
        let d = Dependency {
            owner: s(r, 1),
            name: s(r, 2),
            object_type: s(r, 3),
        };
        if s(r, 0) == "USES" {
            uses.push(d)
        } else {
            used_by.push(d)
        }
    }

    Ok(TableInsight {
        owner: owner.into(),
        table: table.into(),
        comment,
        columns,
        indexes,
        triggers,
        referenced_by,
        uses,
        used_by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn t(cols: &[&str], rows: Vec<Vec<Option<&str>>>) -> FakeTable {
        FakeTable::new(cols, rows)
    }

    /// EMP(ID pk, EMAIL unique, DEPT_ID fk->DEPT.ID, SAL check, NAME not null)
    fn emp_db() -> FakeDb {
        let n = None;
        FakeDb::new()
            .with_table(
                "insight:tabcomment",
                t(&["C"], vec![vec![Some("Employees")]]),
            )
            .with_table(
                "insight:cols",
                t(
                    &[
                        "n", "t", "dl", "cl", "cu", "p", "sc", "nl", "cm", "nd", "nn", "h",
                    ],
                    vec![
                        vec![
                            Some("ID"),
                            Some("NUMBER"),
                            Some("22"),
                            n,
                            n,
                            Some("10"),
                            Some("0"),
                            Some("N"),
                            Some("Primary key"),
                            Some("1000"),
                            Some("0"),
                            Some("NONE"),
                        ],
                        vec![
                            Some("EMAIL"),
                            Some("VARCHAR2"),
                            Some("200"),
                            Some("50"),
                            Some("C"),
                            n,
                            n,
                            Some("Y"),
                            n,
                            Some("990"),
                            Some("10"),
                            Some("NONE"),
                        ],
                        vec![
                            Some("DEPT_ID"),
                            Some("NUMBER"),
                            Some("22"),
                            n,
                            n,
                            Some("6"),
                            Some("0"),
                            Some("Y"),
                            n,
                            Some("11"),
                            Some("3"),
                            Some("FREQUENCY"),
                        ],
                        vec![
                            Some("SAL"),
                            Some("NUMBER"),
                            Some("22"),
                            n,
                            n,
                            Some("8"),
                            Some("2"),
                            Some("Y"),
                            n,
                            n,
                            n,
                            n,
                        ],
                        vec![
                            Some("NAME"),
                            Some("VARCHAR2"),
                            Some("30"),
                            Some("30"),
                            Some("B"),
                            n,
                            n,
                            Some("N"),
                            n,
                            n,
                            n,
                            n,
                        ],
                    ],
                ),
            )
            .with_table(
                "insight:cons",
                t(
                    &["cn", "ct", "col", "st", "cond", "ro", "rt", "rc"],
                    vec![
                        vec![
                            Some("EMP_PK"),
                            Some("P"),
                            Some("ID"),
                            Some("ENABLED"),
                            n,
                            n,
                            n,
                            n,
                        ],
                        vec![
                            Some("EMP_EMAIL_UK"),
                            Some("U"),
                            Some("EMAIL"),
                            Some("ENABLED"),
                            n,
                            n,
                            n,
                            n,
                        ],
                        vec![
                            Some("EMP_DEPT_FK"),
                            Some("R"),
                            Some("DEPT_ID"),
                            Some("ENABLED"),
                            n,
                            Some("HR"),
                            Some("DEPT"),
                            Some("ID"),
                        ],
                        vec![
                            Some("EMP_SAL_CK"),
                            Some("C"),
                            Some("SAL"),
                            Some("ENABLED"),
                            Some("sal > 0"),
                            n,
                            n,
                            n,
                        ],
                        vec![
                            Some("SYS_C001"),
                            Some("C"),
                            Some("NAME"),
                            Some("ENABLED"),
                            Some("\"NAME\" IS NOT NULL"),
                            n,
                            n,
                            n,
                        ],
                    ],
                ),
            )
            .with_table(
                "insight:idx",
                t(
                    &["i", "u", "c", "p", "s", "t"],
                    vec![
                        vec![
                            Some("EMP_PK"),
                            Some("UNIQUE"),
                            Some("ID"),
                            Some("1"),
                            Some("VALID"),
                            Some("NORMAL"),
                        ],
                        vec![
                            Some("EMP_DEPT_SAL_IX"),
                            Some("NONUNIQUE"),
                            Some("DEPT_ID"),
                            Some("1"),
                            Some("UNUSABLE"),
                            Some("NORMAL"),
                        ],
                        vec![
                            Some("EMP_DEPT_SAL_IX"),
                            Some("NONUNIQUE"),
                            Some("SAL"),
                            Some("2"),
                            Some("UNUSABLE"),
                            Some("NORMAL"),
                        ],
                    ],
                ),
            )
            .with_table(
                "insight:trg",
                t(
                    &["tn", "tt", "ev", "st", "col", "cl", "cu"],
                    vec![
                        // BEFORE UPDATE OF SAL: restricted to SAL
                        vec![
                            Some("EMP_SAL_AUDIT"),
                            Some("BEFORE EACH ROW"),
                            Some("UPDATE"),
                            Some("ENABLED"),
                            Some("SAL"),
                            Some("YES"),
                            Some("NEW OLD"),
                        ],
                        // AFTER INSERT OR UPDATE: references NAME, fires on any update
                        vec![
                            Some("EMP_LOG"),
                            Some("AFTER EACH ROW"),
                            Some("INSERT OR UPDATE"),
                            Some("DISABLED"),
                            Some("NAME"),
                            Some("NO"),
                            Some("NEW"),
                        ],
                    ],
                ),
            )
            .with_table(
                "insight:children",
                t(
                    &["o", "t", "c"],
                    vec![vec![Some("HR"), Some("JOB_HISTORY"), Some("JH_EMP_FK")]],
                ),
            )
            .with_table(
                "insight:deps",
                t(
                    &["d", "o", "n", "t"],
                    vec![
                        vec![Some("USES"), Some("HR"), Some("DEPT"), Some("TABLE")],
                        vec![Some("USED_BY"), Some("HR"), Some("EMP_V"), Some("VIEW")],
                    ],
                ),
            )
    }

    fn col<'a>(i: &'a TableInsight, n: &str) -> &'a ColumnInsight {
        i.columns.iter().find(|c| c.name == n).unwrap()
    }

    #[test]
    fn keys_constraints_and_types() {
        let i = table_insight(&emp_db(), "HR", "EMP").unwrap();
        assert_eq!(i.comment.as_deref(), Some("Employees"));
        assert!(col(&i, "ID").primary_key && !col(&i, "ID").nullable);
        assert_eq!(col(&i, "ID").data_type, "NUMBER(10)");
        assert!(col(&i, "EMAIL").unique);
        assert_eq!(col(&i, "EMAIL").data_type, "VARCHAR2(50 CHAR)");
        assert_eq!(
            col(&i, "DEPT_ID").foreign_keys,
            vec![FkRef {
                constraint: "EMP_DEPT_FK".into(),
                ref_owner: "HR".into(),
                ref_table: "DEPT".into(),
                ref_column: "ID".into()
            }]
        );
        assert_eq!(col(&i, "SAL").checks, vec!["sal > 0".to_string()]);
        assert!(
            col(&i, "NAME").checks.is_empty(),
            "NOT NULL is not reported as a check"
        );
    }

    #[test]
    fn stats_and_comments() {
        let i = table_insight(&emp_db(), "HR", "EMP").unwrap();
        assert_eq!(col(&i, "ID").comment.as_deref(), Some("Primary key"));
        assert_eq!(
            (col(&i, "EMAIL").distinct, col(&i, "EMAIL").nulls),
            (Some(990), Some(10))
        );
        assert_eq!(col(&i, "DEPT_ID").histogram.as_deref(), Some("FREQUENCY"));
        assert_eq!(col(&i, "ID").histogram, None, "NONE is hidden");
        assert_eq!(col(&i, "SAL").distinct, None);
    }

    #[test]
    fn indexes_report_position_and_leading() {
        let i = table_insight(&emp_db(), "HR", "EMP").unwrap();
        assert_eq!(i.indexes.len(), 2);
        assert_eq!(i.indexes[1].columns, vec!["DEPT_ID", "SAL"]);
        assert!(!i.indexes[1].valid, "UNUSABLE index flagged");
        let sal = &col(&i, "SAL").indexes[0];
        assert_eq!((sal.position, sal.leading, sal.valid), (2, false, false));
        assert!(col(&i, "DEPT_ID").indexes[0].leading);
        assert!(col(&i, "ID").indexes[0].unique);
        assert!(col(&i, "NAME").indexes.is_empty());
    }

    #[test]
    fn trigger_attribution_rules() {
        let i = table_insight(&emp_db(), "HR", "EMP").unwrap();
        let r = |c: &str| {
            col(&i, c)
                .triggers
                .iter()
                .map(|t| (t.name.as_str(), t.reason.as_str(), t.enabled))
                .collect::<Vec<_>>()
        };
        // UPDATE OF SAL trigger only attaches to SAL; EMP_LOG fires on any update.
        assert_eq!(
            r("SAL"),
            vec![
                ("EMP_SAL_AUDIT", "update_of", true),
                ("EMP_LOG", "any_update", false)
            ]
        );
        assert_eq!(r("NAME"), vec![("EMP_LOG", "referenced", false)]);
        assert_eq!(r("ID"), vec![("EMP_LOG", "any_update", false)]);
        assert_eq!(i.triggers[0].timing, "BEFORE (row)");
        assert_eq!(i.triggers[0].columns, vec!["SAL"]);
    }

    #[test]
    fn children_and_dependencies() {
        let i = table_insight(&emp_db(), "HR", "EMP").unwrap();
        assert_eq!(i.referenced_by[0].table, "JOB_HISTORY");
        assert_eq!(i.uses[0].name, "DEPT");
        assert_eq!(i.used_by[0].object_type, "VIEW");
    }

    #[test]
    fn identifiers_are_always_bound() {
        let db = emp_db();
        table_insight(&db, "HR", "EMP\" OR 1=1 --").unwrap();
        let log = db.log.lock().unwrap();
        assert!(log
            .calls
            .iter()
            .all(|(sql, binds)| !sql.contains("OR 1=1") && binds.len() == 2));
    }

    #[test]
    fn unknown_table_yields_empty_insight_not_error() {
        let i = table_insight(
            &FakeDb::new().with_table("insight:", t(&["x"], vec![])),
            "HR",
            "NOPE",
        )
        .unwrap();
        assert!(i.columns.is_empty() && i.indexes.is_empty() && i.triggers.is_empty());
    }
}
