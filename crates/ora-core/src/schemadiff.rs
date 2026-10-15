//! Schema comparison between two schemas (usually on two databases): tables, columns, keys, indexes,
//! triggers, sequences and the *text* of code objects. **A is the reference; the draft script makes
//! B look like A.** The script is only text: it is never run, destructive statements are commented
//! out, and defaults, storage and partitioning are not part of the comparison.
//!
//! Constraints and indexes are matched by what they are (kind, columns, target), not by name, so
//! system-generated names (`SYS_C0012345`) do not show up as differences.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{collect, Database, Row};
use crate::knowledge::{ConFacts, IdxFacts, Knowledge, SeqFacts, TableFacts};
use crate::sqltext::redact_secrets;
use crate::textdiff::mask_sql_comments;
use crate::Result;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Change {
    /// `only_a` (missing in B), `only_b` (extra in B) or `differs`.
    pub change: String,
    /// What it is: `column`, `primary key`, `index`…
    pub what: String,
    pub name: String,
    pub a: Option<String>,
    pub b: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableDiff {
    pub name: String,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodeDiff {
    pub kind: String,
    pub name: String,
    /// `only_a`, `only_b` or `differs`.
    pub change: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SchemaSummary {
    pub tables_only_a: usize,
    pub tables_only_b: usize,
    pub tables_differ: usize,
    pub column_changes: usize,
    pub other_changes: usize,
    pub code_only_a: usize,
    pub code_only_b: usize,
    pub code_differs: usize,
    pub identical: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchemaDiff {
    pub owner_a: String,
    pub owner_b: String,
    pub tables_only_a: Vec<String>,
    pub tables_only_b: Vec<String>,
    pub tables: Vec<TableDiff>,
    pub sequences: Vec<Change>,
    pub code: Vec<CodeDiff>,
    /// `false` when code text was not compared.
    pub code_compared: bool,
    pub summary: SchemaSummary,
    /// Draft script that makes B look like A. Review before running anything.
    pub script: String,
    pub notes: Vec<String>,
}

/// Hash of each code object's text with comments, case and spacing ignored.
#[derive(Debug, Clone, Default)]
pub struct CodeHashes(pub BTreeMap<(String, String), String>);

pub fn code_hash(text: &str) -> String {
    let masked = mask_sql_comments(text);
    let norm = masked
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    Sha256::digest(norm.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

/// Reads PL/SQL source (`ALL_SOURCE`) and view text of one schema and hashes each object.
pub fn load_code_hashes(db: &dyn Database, owner: &str) -> Result<CodeHashes> {
    let b = [owner.to_string()];
    let mut text: BTreeMap<(String, String), String> = BTreeMap::new();
    let (_, rows) = collect(
        db,
        "SELECT /*sd:src*/ type, name, text FROM all_source WHERE owner = :1 ORDER BY type, name, line",
        &b,
    )?;
    for r in &rows {
        text.entry((s(r, 0), s(r, 1)))
            .or_default()
            .push_str(&s(r, 2));
    }
    // 12.2+; older databases simply have no view text here.
    if let Ok((_, rows)) = collect(
        db,
        "SELECT /*sd:views*/ view_name, text_vc FROM all_views WHERE owner = :1",
        &b,
    ) {
        for r in &rows {
            text.insert(("VIEW".into(), s(r, 0)), s(r, 1));
        }
    }
    Ok(CodeHashes(
        text.into_iter()
            .map(|(k, v)| (k, code_hash(&redact_secrets(&v))))
            .collect(),
    ))
}

// ------------------------------------------------------------------------------ comparison

fn col_def(c: &crate::knowledge::ColFacts) -> String {
    format!(
        "{} {}",
        c.data_type,
        if c.nullable { "NULL" } else { "NOT NULL" }
    )
}

fn con_key(c: &ConFacts, owner: &str) -> String {
    let target = c
        .ref_table
        .as_ref()
        .map(|r| {
            format!(
                " -> {}({})",
                r.strip_prefix(&format!("{owner}.")).unwrap_or(r),
                c.ref_columns.join(",")
            )
        })
        .unwrap_or_default();
    format!("{}:{}{}", c.kind, c.columns.join(","), target)
}

fn con_what(kind: &str) -> &'static str {
    match kind {
        "P" => "primary key",
        "U" => "unique constraint",
        _ => "foreign key",
    }
}

fn idx_key(i: &IdxFacts) -> String {
    format!(
        "{}:{}",
        if i.unique { "unique" } else { "index" },
        i.columns.join(",")
    )
}

fn seq_def(q: &SeqFacts) -> String {
    format!(
        "increment {} min {} max {} cache {} {}",
        q.increment,
        q.min,
        q.max,
        q.cache,
        if q.cycle { "cycle" } else { "nocycle" }
    )
}

fn system_name(n: &str) -> bool {
    n.starts_with("SYS_C") || n.starts_with("SYS_") && n.len() > 8
}

fn table_changes(a: &TableFacts, b: &TableFacts, oa: &str, ob: &str) -> Vec<Change> {
    let mut out = Vec::new();
    let bcols: BTreeMap<&str, &crate::knowledge::ColFacts> =
        b.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let acols: BTreeSet<&str> = a.columns.iter().map(|c| c.name.as_str()).collect();
    for c in &a.columns {
        match bcols.get(c.name.as_str()) {
            None => out.push(Change {
                change: "only_a".into(),
                what: "column".into(),
                name: c.name.clone(),
                a: Some(col_def(c)),
                b: None,
            }),
            Some(x) if col_def(c) != col_def(x) => out.push(Change {
                change: "differs".into(),
                what: "column".into(),
                name: c.name.clone(),
                a: Some(col_def(c)),
                b: Some(col_def(x)),
            }),
            _ => {}
        }
    }
    for c in &b.columns {
        if !acols.contains(c.name.as_str()) {
            out.push(Change {
                change: "only_b".into(),
                what: "column".into(),
                name: c.name.clone(),
                a: None,
                b: Some(col_def(c)),
            });
        }
    }
    fn by_key<'a>(t: &'a TableFacts, owner: &str) -> BTreeMap<String, &'a ConFacts> {
        t.constraints
            .iter()
            .map(|c| (con_key(c, owner), c))
            .collect()
    }
    let (ka, kb) = (by_key(a, oa), by_key(b, ob));
    for (k, c) in &ka {
        if !kb.contains_key(k) {
            out.push(Change {
                change: "only_a".into(),
                what: con_what(&c.kind).into(),
                name: c.name.clone(),
                a: Some(k.clone()),
                b: None,
            });
        }
    }
    for (k, c) in &kb {
        if !ka.contains_key(k) {
            out.push(Change {
                change: "only_b".into(),
                what: con_what(&c.kind).into(),
                name: c.name.clone(),
                a: None,
                b: Some(k.clone()),
            });
        }
    }
    let ia: BTreeMap<String, &IdxFacts> = a.indexes.iter().map(|i| (idx_key(i), i)).collect();
    let ib: BTreeMap<String, &IdxFacts> = b.indexes.iter().map(|i| (idx_key(i), i)).collect();
    for (k, i) in &ia {
        if !ib.contains_key(k) {
            out.push(Change {
                change: "only_a".into(),
                what: "index".into(),
                name: i.name.clone(),
                a: Some(format!(
                    "{} ({})",
                    if i.unique { "unique" } else { "non-unique" },
                    i.columns.join(", ")
                )),
                b: None,
            });
        }
    }
    for (k, i) in &ib {
        if !ia.contains_key(k) {
            out.push(Change {
                change: "only_b".into(),
                what: "index".into(),
                name: i.name.clone(),
                a: None,
                b: Some(format!(
                    "{} ({})",
                    if i.unique { "unique" } else { "non-unique" },
                    i.columns.join(", ")
                )),
            });
        }
    }
    out
}

fn trigger_changes(a: &Knowledge, b: &Knowledge) -> BTreeMap<String, Vec<Change>> {
    let mut out: BTreeMap<String, Vec<Change>> = BTreeMap::new();
    let bt: BTreeMap<&str, &crate::knowledge::TriggerFacts> =
        b.triggers.iter().map(|t| (t.name.as_str(), t)).collect();
    let at: BTreeSet<&str> = a.triggers.iter().map(|t| t.name.as_str()).collect();
    for t in &a.triggers {
        let table = t.table.clone().unwrap_or_default();
        let def = format!(
            "{} {} ({})",
            t.trigger_type.to_lowercase(),
            t.event.to_lowercase(),
            t.status.to_lowercase()
        );
        match bt.get(t.name.as_str()) {
            None => out.entry(table).or_default().push(Change {
                change: "only_a".into(),
                what: "trigger".into(),
                name: t.name.clone(),
                a: Some(def),
                b: None,
            }),
            Some(x) => {
                let xdef = format!(
                    "{} {} ({})",
                    x.trigger_type.to_lowercase(),
                    x.event.to_lowercase(),
                    x.status.to_lowercase()
                );
                if x.table != t.table || xdef != def {
                    out.entry(table).or_default().push(Change {
                        change: "differs".into(),
                        what: "trigger".into(),
                        name: t.name.clone(),
                        a: Some(def),
                        b: Some(xdef),
                    });
                }
            }
        }
    }
    for t in &b.triggers {
        if !at.contains(t.name.as_str()) {
            out.entry(t.table.clone().unwrap_or_default())
                .or_default()
                .push(Change {
                    change: "only_b".into(),
                    what: "trigger".into(),
                    name: t.name.clone(),
                    a: None,
                    b: Some(format!(
                        "{} {}",
                        t.trigger_type.to_lowercase(),
                        t.event.to_lowercase()
                    )),
                });
        }
    }
    out
}

fn is_code_kind(k: &str) -> bool {
    matches!(
        k,
        "PACKAGE"
            | "PACKAGE BODY"
            | "PROCEDURE"
            | "FUNCTION"
            | "TRIGGER"
            | "TYPE"
            | "TYPE BODY"
            | "VIEW"
    )
}

pub fn compare_schemas(
    a: &Knowledge,
    b: &Knowledge,
    code_a: Option<&CodeHashes>,
    code_b: Option<&CodeHashes>,
) -> SchemaDiff {
    let kind_ok = |t: &TableFacts| t.kind == "TABLE";
    let ta: BTreeMap<&str, &TableFacts> = a
        .tables
        .iter()
        .filter(|t| kind_ok(t))
        .map(|t| (t.name.as_str(), t))
        .collect();
    let tb: BTreeMap<&str, &TableFacts> = b
        .tables
        .iter()
        .filter(|t| kind_ok(t))
        .map(|t| (t.name.as_str(), t))
        .collect();
    let tables_only_a: Vec<String> = ta
        .keys()
        .filter(|k| !tb.contains_key(*k))
        .map(|k| k.to_string())
        .collect();
    let tables_only_b: Vec<String> = tb
        .keys()
        .filter(|k| !ta.contains_key(*k))
        .map(|k| k.to_string())
        .collect();
    let mut trg = trigger_changes(a, b);
    let mut tables = Vec::new();
    for (name, x) in &ta {
        if let Some(y) = tb.get(name) {
            let mut changes = table_changes(x, y, &a.owner, &b.owner);
            changes.extend(trg.remove(*name).unwrap_or_default());
            if !changes.is_empty() {
                tables.push(TableDiff {
                    name: name.to_string(),
                    changes,
                });
            }
        }
    }
    // Triggers of tables that exist on one side only, or schema-level ones.
    let leftover: Vec<Change> = trg.into_values().flatten().collect();

    let mut sequences = Vec::new();
    let sb: BTreeMap<&str, &SeqFacts> = b.sequences.iter().map(|q| (q.name.as_str(), q)).collect();
    let sa: BTreeSet<&str> = a.sequences.iter().map(|q| q.name.as_str()).collect();
    for q in &a.sequences {
        match sb.get(q.name.as_str()) {
            None => sequences.push(Change {
                change: "only_a".into(),
                what: "sequence".into(),
                name: q.name.clone(),
                a: Some(seq_def(q)),
                b: None,
            }),
            Some(y) if seq_def(y) != seq_def(q) => sequences.push(Change {
                change: "differs".into(),
                what: "sequence".into(),
                name: q.name.clone(),
                a: Some(seq_def(q)),
                b: Some(seq_def(y)),
            }),
            _ => {}
        }
    }
    for q in &b.sequences {
        if !sa.contains(q.name.as_str()) {
            sequences.push(Change {
                change: "only_b".into(),
                what: "sequence".into(),
                name: q.name.clone(),
                a: None,
                b: Some(seq_def(q)),
            });
        }
    }

    let mut code = Vec::new();
    let code_compared = code_a.is_some() && code_b.is_some();
    if let (Some(ca), Some(cb)) = (code_a, code_b) {
        for ((k, n), h) in &ca.0 {
            if !is_code_kind(k) {
                continue;
            }
            match cb.0.get(&(k.clone(), n.clone())) {
                None => code.push(CodeDiff {
                    kind: k.clone(),
                    name: n.clone(),
                    change: "only_a".into(),
                }),
                Some(x) if x != h => code.push(CodeDiff {
                    kind: k.clone(),
                    name: n.clone(),
                    change: "differs".into(),
                }),
                _ => {}
            }
        }
        for (k, n) in cb.0.keys() {
            if is_code_kind(k) && !ca.0.contains_key(&(k.clone(), n.clone())) {
                code.push(CodeDiff {
                    kind: k.clone(),
                    name: n.clone(),
                    change: "only_b".into(),
                });
            }
        }
    }

    let mut sum = SchemaSummary {
        tables_only_a: tables_only_a.len(),
        tables_only_b: tables_only_b.len(),
        tables_differ: tables.len(),
        column_changes: tables
            .iter()
            .flat_map(|t| &t.changes)
            .filter(|c| c.what == "column")
            .count(),
        other_changes: tables
            .iter()
            .flat_map(|t| &t.changes)
            .filter(|c| c.what != "column")
            .count()
            + sequences.len()
            + leftover.len(),
        code_only_a: code.iter().filter(|c| c.change == "only_a").count(),
        code_only_b: code.iter().filter(|c| c.change == "only_b").count(),
        code_differs: code.iter().filter(|c| c.change == "differs").count(),
        identical: false,
    };
    sum.identical = sum.tables_only_a
        + sum.tables_only_b
        + sum.tables_differ
        + sum.other_changes
        + sum.code_only_a
        + sum.code_only_b
        + sum.code_differs
        == 0;
    let mut notes = vec![
        "A is the reference: the script makes B look like A. Columns are matched by name, constraints and indexes by definition (names are ignored when they are system-generated).".to_string(),
        "Not compared: default values, storage, partitioning, grants, comments, column order.".to_string(),
    ];
    if !code_compared {
        notes
            .push("Code objects (packages, procedures, triggers, views) were not compared.".into());
    } else {
        notes.push("Code is compared by its text with comments, case and spacing ignored.".into());
    }
    let script = draft_script(a, b, &ta, &tb, &tables_only_a, &tables, &sequences, &code);
    SchemaDiff {
        owner_a: a.owner.clone(),
        owner_b: b.owner.clone(),
        tables_only_a,
        tables_only_b,
        tables,
        sequences,
        code,
        code_compared,
        summary: sum,
        script,
        notes,
    }
}

// ----------------------------------------------------------------------------------- script

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn constraint_clause(c: &ConFacts, a_owner: &str, b_owner: &str) -> String {
    let name = if system_name(&c.name) {
        String::new()
    } else {
        format!("CONSTRAINT {} ", q(&c.name))
    };
    let cols = c
        .columns
        .iter()
        .map(|x| q(x))
        .collect::<Vec<_>>()
        .join(", ");
    match c.kind.as_str() {
        "P" => format!("{name}PRIMARY KEY ({cols})"),
        "U" => format!("{name}UNIQUE ({cols})"),
        _ => {
            let r = c.ref_table.clone().unwrap_or_default();
            let (ro, rn) = r.split_once('.').unwrap_or((a_owner, &r));
            let ro = if ro == a_owner { b_owner } else { ro };
            format!(
                "{name}FOREIGN KEY ({cols}) REFERENCES {}.{} ({})",
                q(ro),
                q(rn),
                c.ref_columns
                    .iter()
                    .map(|x| q(x))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draft_script(
    a: &Knowledge,
    b: &Knowledge,
    ta: &BTreeMap<&str, &TableFacts>,
    tb: &BTreeMap<&str, &TableFacts>,
    only_a: &[String],
    tables: &[TableDiff],
    seqs: &[Change],
    code: &[CodeDiff],
) -> String {
    let ob = &b.owner;
    let mut out = String::new();
    out.push_str(&format!("-- Draft: make {ob} look like {}. REVIEW BEFORE RUNNING; nothing here has been executed.\n", a.owner));
    out.push_str("-- Destructive statements are commented out. Defaults, storage, partitioning and grants are not included.\n\n");
    let mut destructive: Vec<String> = Vec::new();
    for sq in seqs {
        let seq = a.sequences.iter().find(|x| x.name == sq.name);
        match (sq.change.as_str(), seq) {
            ("only_a", Some(x)) => out.push_str(&format!(
                "CREATE SEQUENCE {}.{} INCREMENT BY {} MINVALUE {} MAXVALUE {} {} CACHE {};\n",
                q(ob),
                q(&x.name),
                x.increment,
                x.min,
                x.max,
                if x.cycle { "CYCLE" } else { "NOCYCLE" },
                x.cache
            )),
            ("differs", Some(x)) => out.push_str(&format!(
                "ALTER SEQUENCE {}.{} INCREMENT BY {} MINVALUE {} MAXVALUE {} {} CACHE {};\n",
                q(ob),
                q(&x.name),
                x.increment,
                x.min,
                x.max,
                if x.cycle { "CYCLE" } else { "NOCYCLE" },
                x.cache
            )),
            ("only_b", _) => destructive.push(format!("DROP SEQUENCE {}.{};", q(ob), q(&sq.name))),
            _ => {}
        }
    }
    for name in only_a {
        let Some(t) = ta.get(name.as_str()) else {
            continue;
        };
        let mut cols: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                format!(
                    "  {} {}{}",
                    q(&c.name),
                    c.data_type,
                    if c.nullable { "" } else { " NOT NULL" }
                )
            })
            .collect();
        for c in t.constraints.iter().filter(|c| c.kind != "R") {
            cols.push(format!("  {}", constraint_clause(c, &a.owner, ob)));
        }
        out.push_str(&format!(
            "\nCREATE TABLE {}.{} (\n{}\n);\n",
            q(ob),
            q(name),
            cols.join(",\n")
        ));
        for i in &t.indexes {
            if !t
                .constraints
                .iter()
                .any(|c| c.kind != "R" && c.columns == i.columns)
            {
                out.push_str(&format!(
                    "CREATE {}INDEX {}.{} ON {}.{} ({});\n",
                    if i.unique { "UNIQUE " } else { "" },
                    q(ob),
                    q(&i.name),
                    q(ob),
                    q(name),
                    i.columns
                        .iter()
                        .map(|x| q(x))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for c in t.constraints.iter().filter(|c| c.kind == "R") {
            out.push_str(&format!(
                "ALTER TABLE {}.{} ADD {};\n",
                q(ob),
                q(name),
                constraint_clause(c, &a.owner, ob)
            ));
        }
    }
    for td in tables {
        let (Some(x), Some(y)) = (ta.get(td.name.as_str()), tb.get(td.name.as_str())) else {
            continue;
        };
        let tn = format!("{}.{}", q(ob), q(&td.name));
        out.push_str(&format!("\n-- {}\n", td.name));
        for ch in &td.changes {
            match (ch.what.as_str(), ch.change.as_str()) {
                ("column", "only_a") => {
                    let Some(c) = x.columns.iter().find(|c| c.name == ch.name) else {
                        continue;
                    };
                    out.push_str(&format!(
                        "ALTER TABLE {tn} ADD ({} {});\n",
                        q(&c.name),
                        c.data_type
                    ));
                    if !c.nullable {
                        out.push_str(&format!("-- after filling existing rows: ALTER TABLE {tn} MODIFY ({} NOT NULL);\n", q(&c.name)));
                    }
                }
                ("column", "differs") => {
                    let (Some(c), Some(d)) = (
                        x.columns.iter().find(|c| c.name == ch.name),
                        y.columns.iter().find(|c| c.name == ch.name),
                    ) else {
                        continue;
                    };
                    let mut parts = String::new();
                    if c.data_type != d.data_type {
                        parts.push_str(&format!(" {}", c.data_type));
                    }
                    if c.nullable != d.nullable {
                        parts.push_str(if c.nullable { " NULL" } else { " NOT NULL" });
                    }
                    out.push_str(&format!("-- review: fails if existing data does not fit\nALTER TABLE {tn} MODIFY ({}{parts});\n", q(&c.name)));
                }
                ("column", "only_b") => {
                    destructive.push(format!("ALTER TABLE {tn} DROP COLUMN {};", q(&ch.name)))
                }
                ("index", "only_a") => {
                    if let Some(i) = x.indexes.iter().find(|i| i.name == ch.name) {
                        out.push_str(&format!(
                            "CREATE {}INDEX {}.{} ON {tn} ({});\n",
                            if i.unique { "UNIQUE " } else { "" },
                            q(ob),
                            q(&i.name),
                            i.columns
                                .iter()
                                .map(|c| q(c))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                ("index", "only_b") => {
                    destructive.push(format!("DROP INDEX {}.{};", q(ob), q(&ch.name)))
                }
                ("primary key" | "unique constraint" | "foreign key", "only_a") => {
                    if let Some(c) = x.constraints.iter().find(|c| c.name == ch.name) {
                        out.push_str(&format!(
                            "ALTER TABLE {tn} ADD {};\n",
                            constraint_clause(c, &a.owner, ob)
                        ));
                    }
                }
                ("primary key" | "unique constraint" | "foreign key", "only_b") => {
                    destructive.push(format!("ALTER TABLE {tn} DROP CONSTRAINT {};", q(&ch.name)))
                }
                ("trigger", _) => out.push_str(&format!(
                    "-- trigger {} {}: compare its source below\n",
                    ch.name,
                    if ch.change == "only_a" {
                        "is missing in B"
                    } else if ch.change == "only_b" {
                        "exists only in B"
                    } else {
                        "differs"
                    }
                )),
                _ => {}
            }
        }
    }
    if !code.is_empty() {
        out.push_str(
            "\n-- Code objects (compare the text in the app; create or replace them from A):\n",
        );
        for c in code {
            out.push_str(&format!(
                "-- {} {} {}\n",
                c.kind.to_lowercase(),
                c.name,
                match c.change.as_str() {
                    "only_a" => "missing in B",
                    "only_b" => "exists only in B",
                    _ => "differs",
                }
            ));
        }
    }
    if !destructive.is_empty() || !b.tables.is_empty() {
        let extra: Vec<String> = tb
            .keys()
            .filter(|k| !ta.contains_key(*k))
            .map(|k| format!("DROP TABLE {}.{};", q(ob), q(k)))
            .collect();
        if !destructive.is_empty() || !extra.is_empty() {
            out.push_str("\n-- Destructive: only in B. Uncomment deliberately.\n");
            for d in destructive.iter().chain(extra.iter()) {
                out.push_str(&format!("-- {d}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::ColFacts;

    fn col(n: &str, t: &str, null: bool) -> ColFacts {
        ColFacts {
            name: n.into(),
            data_type: t.into(),
            nullable: null,
            comment: None,
        }
    }
    fn con(name: &str, kind: &str, cols: &[&str], r: Option<(&str, &str)>) -> ConFacts {
        ConFacts {
            name: name.into(),
            kind: kind.into(),
            status: "ENABLED".into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
            ref_table: r.map(|x| x.0.to_string()),
            ref_columns: r.map(|x| vec![x.1.to_string()]).unwrap_or_default(),
        }
    }
    fn idx(name: &str, unique: bool, cols: &[&str]) -> IdxFacts {
        IdxFacts {
            name: name.into(),
            unique,
            index_type: "NORMAL".into(),
            status: "VALID".into(),
            columns: cols.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn table(
        name: &str,
        cols: Vec<ColFacts>,
        cons: Vec<ConFacts>,
        ix: Vec<IdxFacts>,
    ) -> TableFacts {
        TableFacts {
            name: name.into(),
            kind: "TABLE".into(),
            status: "VALID".into(),
            comment: None,
            num_rows: None,
            last_analyzed: None,
            columns: cols,
            constraints: cons,
            indexes: ix,
        }
    }
    fn kb(owner: &str, tables: Vec<TableFacts>) -> Knowledge {
        let mut k = Knowledge::empty(owner);
        k.tables = tables;
        k
    }

    fn pair() -> (Knowledge, Knowledge) {
        let a = kb(
            "DEV",
            vec![
                table(
                    "EMP",
                    vec![
                        col("ID", "NUMBER(6)", false),
                        col("NAME", "VARCHAR2(40 CHAR)", true),
                        col("DEPT_ID", "NUMBER(4)", true),
                        col("EMAIL", "VARCHAR2(80)", false),
                    ],
                    vec![
                        con("EMP_PK", "P", &["ID"], None),
                        con("EMP_DEPT_FK", "R", &["DEPT_ID"], Some(("DEV.DEPT", "ID"))),
                    ],
                    vec![
                        idx("EMP_PK", true, &["ID"]),
                        idx("EMP_DEPT_IX", false, &["DEPT_ID"]),
                    ],
                ),
                table(
                    "DEPT",
                    vec![col("ID", "NUMBER(4)", false)],
                    vec![con("DEPT_PK", "P", &["ID"], None)],
                    vec![idx("DEPT_PK", true, &["ID"])],
                ),
                table(
                    "NEWT",
                    vec![col("ID", "NUMBER", false), col("V", "VARCHAR2(10)", true)],
                    vec![con("SYS_C0011", "P", &["ID"], None)],
                    vec![idx("SYS_C0011", true, &["ID"])],
                ),
            ],
        );
        let b = kb(
            "PROD",
            vec![
                table(
                    "EMP",
                    vec![
                        col("ID", "NUMBER(6)", false),
                        col("NAME", "VARCHAR2(20 CHAR)", true),
                        col("DEPT_ID", "NUMBER(4)", true),
                        col("LEGACY", "DATE", true),
                    ],
                    vec![con("SYS_C99", "P", &["ID"], None)],
                    vec![
                        idx("SYS_IL1", true, &["ID"]),
                        idx("EMP_OLD_IX", false, &["NAME"]),
                    ],
                ),
                table(
                    "DEPT",
                    vec![col("ID", "NUMBER(4)", false)],
                    vec![con("DEPT_PK", "P", &["ID"], None)],
                    vec![idx("DEPT_PK", true, &["ID"])],
                ),
                table("OLDT", vec![col("X", "NUMBER", true)], vec![], vec![]),
            ],
        );
        (a, b)
    }

    #[test]
    fn differences_are_found_by_definition_not_by_system_generated_names() {
        let (a, b) = pair();
        let d = compare_schemas(&a, &b, None, None);
        assert_eq!(
            (d.tables_only_a.clone(), d.tables_only_b.clone()),
            (vec!["NEWT".to_string()], vec!["OLDT".to_string()])
        );
        assert_eq!(d.tables.len(), 1, "DEPT is identical");
        let emp = &d.tables[0];
        let has = |what: &str, change: &str, name: &str| {
            emp.changes
                .iter()
                .any(|c| c.what == what && c.change == change && c.name == name)
        };
        assert!(has("column", "only_a", "EMAIL") && has("column", "only_b", "LEGACY"));
        assert!(has("column", "differs", "NAME"));
        assert!(has("foreign key", "only_a", "EMP_DEPT_FK"));
        assert!(
            !emp.changes.iter().any(|c| c.what == "primary key"),
            "EMP_PK vs SYS_C99 is the same primary key"
        );
        assert!(has("index", "only_a", "EMP_DEPT_IX") && has("index", "only_b", "EMP_OLD_IX"));
        assert!(
            !emp.changes
                .iter()
                .any(|c| c.name == "SYS_IL1" || c.name == "EMP_PK" && c.what == "index"),
            "same unique index under another name"
        );
        let name = emp.changes.iter().find(|c| c.name == "NAME").unwrap();
        assert_eq!(
            (name.a.as_deref(), name.b.as_deref()),
            (
                Some("VARCHAR2(40 CHAR) NULL"),
                Some("VARCHAR2(20 CHAR) NULL")
            )
        );
        assert!(!d.summary.identical);
    }

    #[test]
    fn the_draft_script_makes_b_like_a_and_comments_out_what_destroys_data() {
        let (a, b) = pair();
        let sql = compare_schemas(&a, &b, None, None).script;
        assert!(
            sql.contains("ALTER TABLE \"PROD\".\"EMP\" ADD (\"EMAIL\" VARCHAR2(80));"),
            "{sql}"
        );
        assert!(sql.contains("-- after filling existing rows: ALTER TABLE \"PROD\".\"EMP\" MODIFY (\"EMAIL\" NOT NULL);"), "NOT NULL on a populated column is deferred: {sql}");
        assert!(
            sql.contains("ALTER TABLE \"PROD\".\"EMP\" MODIFY (\"NAME\" VARCHAR2(40 CHAR));"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "CREATE INDEX \"PROD\".\"EMP_DEPT_IX\" ON \"PROD\".\"EMP\" (\"DEPT_ID\");"
            ),
            "{sql}"
        );
        assert!(sql.contains("ALTER TABLE \"PROD\".\"EMP\" ADD CONSTRAINT \"EMP_DEPT_FK\" FOREIGN KEY (\"DEPT_ID\") REFERENCES \"PROD\".\"DEPT\" (\"ID\");"), "foreign key points at the B schema: {sql}");
        assert!(
            sql.contains("CREATE TABLE \"PROD\".\"NEWT\" (")
                && sql.contains("PRIMARY KEY (\"ID\")")
                && !sql.contains("SYS_C0011"),
            "{sql}"
        );
        for destructive in [
            "DROP COLUMN \"LEGACY\"",
            "DROP INDEX \"PROD\".\"EMP_OLD_IX\"",
            "DROP TABLE \"PROD\".\"OLDT\"",
        ] {
            let line = sql
                .lines()
                .find(|l| l.contains(destructive))
                .unwrap_or_else(|| panic!("{destructive} missing: {sql}"));
            assert!(
                line.starts_with("-- "),
                "destructive statement must be commented out: {line}"
            );
        }
        assert!(sql.starts_with("-- Draft: make PROD look like DEV."));
    }

    #[test]
    fn identical_schemas_are_reported_identical_with_an_empty_script_body() {
        let (a, _) = pair();
        let d = compare_schemas(&a, &a.clone(), None, None);
        assert!(d.summary.identical && d.tables.is_empty());
        assert!(!d.script.contains("ALTER") && !d.script.contains("CREATE"));
    }

    #[test]
    fn sequences_and_triggers_are_compared() {
        let (mut a, mut b) = pair();
        let seq = |n: &str, inc: &str| SeqFacts {
            name: n.into(),
            min: "1".into(),
            max: "999".into(),
            increment: inc.into(),
            cycle: false,
            cache: "20".into(),
            last: "5".into(),
        };
        a.sequences = vec![seq("S1", "1"), seq("S2", "1")];
        b.sequences = vec![seq("S1", "10"), seq("S3", "1")];
        let trg = |n: &str, ev: &str| crate::knowledge::TriggerFacts {
            name: n.into(),
            table: Some("EMP".into()),
            trigger_type: "BEFORE EACH ROW".into(),
            event: ev.into(),
            status: "ENABLED".into(),
        };
        a.triggers = vec![trg("T1", "INSERT"), trg("T2", "UPDATE")];
        b.triggers = vec![trg("T1", "INSERT OR UPDATE")];
        let d = compare_schemas(&a, &b, None, None);
        let kinds: Vec<(&str, &str)> = d
            .sequences
            .iter()
            .map(|c| (c.name.as_str(), c.change.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![("S1", "differs"), ("S2", "only_a"), ("S3", "only_b")]
        );
        let emp = d.tables.iter().find(|t| t.name == "EMP").unwrap();
        assert!(emp
            .changes
            .iter()
            .any(|c| c.what == "trigger" && c.name == "T1" && c.change == "differs"));
        assert!(emp
            .changes
            .iter()
            .any(|c| c.what == "trigger" && c.name == "T2" && c.change == "only_a"));
        assert!(
            d.script.contains("CREATE SEQUENCE \"PROD\".\"S2\"")
                && d.script
                    .contains("ALTER SEQUENCE \"PROD\".\"S1\" INCREMENT BY 1")
                && d.script.contains("-- DROP SEQUENCE \"PROD\".\"S3\";")
        );
    }

    #[test]
    fn code_is_compared_by_text_ignoring_comments_case_and_spacing() {
        assert_eq!(
            code_hash("PROCEDURE p IS\n BEGIN NULL; -- note\nEND;"),
            code_hash("procedure  p is begin null; /* x */ end;")
        );
        assert_ne!(
            code_hash("BEGIN NULL; END;"),
            code_hash("BEGIN x := 1; END;")
        );
        let h = |items: &[(&str, &str, &str)]| {
            CodeHashes(
                items
                    .iter()
                    .map(|(k, n, t)| ((k.to_string(), n.to_string()), code_hash(t)))
                    .collect(),
            )
        };
        let ca = h(&[
            ("PACKAGE BODY", "P", "a"),
            ("PROCEDURE", "ONLY_A", "x"),
            ("VIEW", "V", "select 1"),
        ]);
        let cb = h(&[
            ("PACKAGE BODY", "P", "b"),
            ("FUNCTION", "ONLY_B", "y"),
            ("VIEW", "V", "SELECT  1"),
        ]);
        let (a, b) = pair();
        let d = compare_schemas(&a, &b, Some(&ca), Some(&cb));
        assert!(d.code_compared);
        let got: Vec<(String, String, String)> = d
            .code
            .iter()
            .map(|c| (c.kind.clone(), c.name.clone(), c.change.clone()))
            .collect();
        assert!(got.contains(&("PACKAGE BODY".into(), "P".into(), "differs".into())));
        assert!(
            got.contains(&("PROCEDURE".into(), "ONLY_A".into(), "only_a".into()))
                && got.contains(&("FUNCTION".into(), "ONLY_B".into(), "only_b".into()))
        );
        assert!(
            !got.iter().any(|c| c.1 == "V"),
            "the view differs only in spacing/case"
        );
        assert_eq!(
            (
                d.summary.code_differs,
                d.summary.code_only_a,
                d.summary.code_only_b
            ),
            (1, 1, 1)
        );
        assert!(!compare_schemas(&a, &b, None, None).code_compared);
    }

    #[test]
    fn the_loader_hashes_source_and_view_text_and_tolerates_old_databases() {
        use crate::fake::{FakeDb, FakeTable};
        let db = FakeDb::new().with_table(
            "sd:src",
            FakeTable::new(
                &["t", "n", "x"],
                vec![
                    vec![Some("PROCEDURE"), Some("P"), Some("PROCEDURE p IS\n")],
                    vec![Some("PROCEDURE"), Some("P"), Some("BEGIN NULL; END;\n")],
                ],
            ),
        );
        let h = load_code_hashes(&db, "HR").unwrap();
        assert_eq!(
            h.0[&("PROCEDURE".to_string(), "P".to_string())],
            code_hash("PROCEDURE p IS\nBEGIN NULL; END;\n")
        );
        assert!(
            !h.0.keys().any(|k| k.0 == "VIEW"),
            "no view text on this database"
        );
    }
}
