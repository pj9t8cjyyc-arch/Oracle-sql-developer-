//! Knowledge base of one schema: every table, column, key, index, trigger, sequence, view,
//! synonym, package and job we can read, linked into one graph where **every link records where it
//! came from and how certain it is**.
//!
//! Sources, most to least certain:
//! * `dictionary` (exact): `ALL_DEPENDENCIES`, constraints, indexes, triggers, synonyms, jobs.
//! * `scan` (heuristic): a text scan of package bodies (see [`crate::packages`]); gives member and
//!   line level reads/writes/calls but misses dynamic SQL.
//!
//! What we *cannot* see is listed in [`Coverage`] instead of being silently absent.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::db::{collect, Database, Row};
use crate::graph::{node_id, GEdge, GNode, Graph};
use crate::packages::{self, PackageAnalysis};
use crate::schema::display_type;
use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColFacts {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConFacts {
    pub name: String,
    /// `P`, `U` or `R`.
    pub kind: String,
    pub status: String,
    pub columns: Vec<String>,
    /// Foreign keys: referenced `OWNER.TABLE` and its columns, in the same order.
    pub ref_table: Option<String>,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdxFacts {
    pub name: String,
    pub unique: bool,
    pub index_type: String,
    pub status: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableFacts {
    pub name: String,
    /// `TABLE` or `VIEW` / `MATERIALIZED VIEW`.
    pub kind: String,
    pub status: String,
    pub comment: Option<String>,
    pub num_rows: Option<i64>,
    pub last_analyzed: Option<String>,
    pub columns: Vec<ColFacts>,
    pub constraints: Vec<ConFacts>,
    pub indexes: Vec<IdxFacts>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerFacts {
    pub name: String,
    pub table: Option<String>,
    pub trigger_type: String,
    pub event: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeqFacts {
    pub name: String,
    pub min: String,
    pub max: String,
    pub increment: String,
    pub cycle: bool,
    pub cache: String,
    pub last: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynFacts {
    pub name: String,
    pub target_owner: String,
    pub target: String,
    pub db_link: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjFacts {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub last_ddl: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFacts {
    pub name: String,
    pub job_type: String,
    pub action: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub object: String,
    pub grantee: String,
    pub privilege: String,
}

/// Why two objects are linked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Evidence {
    pub from: String,
    pub to: String,
    pub kind: String,
    /// `dictionary` or `scan`.
    pub source: String,
    /// `exact` or `heuristic`.
    pub confidence: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Gap {
    /// `warning` (something is unknown) or `info`.
    pub severity: String,
    pub area: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub gaps: Vec<Gap>,
    pub packages: usize,
    pub packages_scanned: usize,
    pub packages_wrapped: usize,
    pub members: usize,
    pub dynamic_sql_sites: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Knowledge {
    pub owner: String,
    pub taken_at: i64,
    pub tables: Vec<TableFacts>,
    pub triggers: Vec<TriggerFacts>,
    pub sequences: Vec<SeqFacts>,
    pub synonyms: Vec<SynFacts>,
    pub objects: Vec<ObjFacts>,
    pub jobs: Vec<JobFacts>,
    pub grants: BTreeMap<String, Vec<Grant>>,
    pub packages: PackageAnalysis,
    pub nodes: Vec<GNode>,
    pub edges: Vec<GEdge>,
    pub evidence: Vec<Evidence>,
    pub coverage: Coverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct KbSummary {
    pub owner: String,
    pub taken_at: i64,
    pub tables: usize,
    pub views: usize,
    pub packages: usize,
    pub members: usize,
    pub triggers: usize,
    pub indexes: usize,
    pub sequences: usize,
    pub nodes: usize,
    pub edges: usize,
    pub exact_links: usize,
    pub heuristic_links: usize,
    pub coverage: Coverage,
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.is_empty())
}

impl Knowledge {
    pub fn graph(&self) -> Graph {
        Graph::from_parts(&self.owner, &self.nodes, &self.edges)
    }

    pub fn summary(&self) -> KbSummary {
        let count = |kinds: &[&str]| {
            self.tables
                .iter()
                .filter(|t| kinds.contains(&t.kind.as_str()))
                .count()
        };
        KbSummary {
            owner: self.owner.clone(),
            taken_at: self.taken_at,
            tables: count(&["TABLE"]),
            views: count(&["VIEW", "MATERIALIZED VIEW"]),
            packages: self.packages.packages.len(),
            members: self.coverage.members,
            triggers: self.triggers.len(),
            indexes: self.tables.iter().map(|t| t.indexes.len()).sum(),
            sequences: self.sequences.len(),
            nodes: self.nodes.len(),
            edges: self.edges.len(),
            exact_links: self
                .evidence
                .iter()
                .filter(|e| e.confidence == "exact")
                .count(),
            heuristic_links: self
                .evidence
                .iter()
                .filter(|e| e.confidence != "exact")
                .count(),
            coverage: self.coverage.clone(),
        }
    }

    pub fn table(&self, name: &str) -> Option<&TableFacts> {
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// Evidence for the link between two node ids, both directions.
    pub fn evidence_between<'a>(&'a self, a: &str, b: &str) -> Vec<&'a Evidence> {
        self.evidence
            .iter()
            .filter(|e| (e.from == a && e.to == b) || (e.from == b && e.to == a))
            .collect()
    }
}

fn rank(kind: &str) -> u8 {
    match kind {
        "writes" => 6,
        "reads" => 5,
        "calls" => 4,
        "fk" | "runs" => 3,
        "indexes" => 2,
        _ => 1,
    }
}

#[derive(Default)]
struct Builder {
    nodes: BTreeMap<String, GNode>,
    /// One edge per ordered pair: the most specific kind wins; the evidence keeps every reason.
    edges: BTreeMap<(String, String), String>,
    evidence: Vec<Evidence>,
}

impl Builder {
    fn node(&mut self, kind: &str, owner: &str, name: &str) -> String {
        let id = node_id(kind, owner, name);
        self.nodes.entry(id.clone()).or_insert_with(|| {
            let kind = id.split(':').next().unwrap_or(kind).to_string();
            GNode {
                id: id.clone(),
                owner: owner.to_string(),
                name: name.to_string(),
                kind,
            }
        });
        id
    }

    fn link(&mut self, from: &str, to: &str, kind: &str, source: &str, detail: String) {
        if from == to {
            return;
        }
        let e = self
            .edges
            .entry((from.to_string(), to.to_string()))
            .or_insert_with(|| kind.to_string());
        if rank(kind) > rank(e) {
            *e = kind.to_string();
        }
        let ev = Evidence {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            source: source.to_string(),
            confidence: if source == "dictionary" {
                "exact"
            } else {
                "heuristic"
            }
            .to_string(),
            detail,
        };
        if !self.evidence.contains(&ev) {
            self.evidence.push(ev);
        }
    }
}

/// Reads the whole schema. Optional sources (grants, scheduler jobs) failing is reported in the
/// coverage, never fatal.
pub fn collect_schema(db: &dyn Database, owner: &str, taken_at: i64) -> Result<Knowledge> {
    let b = [owner.to_string()];
    let mut gaps: Vec<Gap> = Vec::new();
    let mut warn = |area: &str, message: String| {
        gaps.push(Gap {
            severity: "warning".into(),
            area: area.into(),
            message,
        })
    };

    // ---- tables and views
    let (_, rows) = collect(
        db,
        "SELECT /*kb:objs*/ object_name, object_type, status, TO_CHAR(last_ddl_time,'YYYY-MM-DD HH24:MI') FROM all_objects \
         WHERE owner = :1 AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','PROCEDURE','FUNCTION','TYPE','SYNONYM','TRIGGER','PACKAGE')",
        &b,
    )?;
    let objects: Vec<ObjFacts> = rows
        .iter()
        .map(|r| ObjFacts {
            name: s(r, 0),
            kind: s(r, 1),
            status: s(r, 2),
            last_ddl: opt(r, 3),
        })
        .collect();
    let mut tables: BTreeMap<String, TableFacts> = objects
        .iter()
        .filter(|o| matches!(o.kind.as_str(), "TABLE" | "VIEW" | "MATERIALIZED VIEW"))
        .map(|o| {
            (
                o.name.clone(),
                TableFacts {
                    name: o.name.clone(),
                    kind: o.kind.clone(),
                    status: o.status.clone(),
                    comment: None,
                    num_rows: None,
                    last_analyzed: None,
                    columns: vec![],
                    constraints: vec![],
                    indexes: vec![],
                },
            )
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT /*kb:tables*/ t.table_name, c.comments, t.num_rows, TO_CHAR(t.last_analyzed,'YYYY-MM-DD HH24:MI') \
         FROM all_tables t LEFT JOIN all_tab_comments c ON c.owner = t.owner AND c.table_name = t.table_name \
         WHERE t.owner = :1",
        &b,
    )?;
    for r in &rows {
        if let Some(t) = tables.get_mut(&s(r, 0)) {
            t.comment = opt(r, 1);
            t.num_rows = s(r, 2).parse().ok();
            t.last_analyzed = opt(r, 3);
        }
    }
    let (_, rows) = collect(
        db,
        "SELECT /*kb:cols*/ c.table_name, c.column_name, c.data_type, c.data_length, c.char_length, c.char_used, c.data_precision, c.data_scale, c.nullable, cm.comments \
         FROM all_tab_cols c LEFT JOIN all_col_comments cm ON cm.owner = c.owner AND cm.table_name = c.table_name AND cm.column_name = c.column_name \
         WHERE c.owner = :1 AND c.hidden_column = 'NO' ORDER BY c.table_name, c.column_id",
        &b,
    )?;
    for r in &rows {
        if let Some(t) = tables.get_mut(&s(r, 0)) {
            t.columns.push(ColFacts {
                name: s(r, 1),
                data_type: display_type(&s(r, 2), &s(r, 3), &s(r, 4), &s(r, 5), &s(r, 6), &s(r, 7)),
                nullable: s(r, 8) != "N",
                comment: opt(r, 9),
            });
        }
    }
    let (_, rows) = collect(
        db,
        "SELECT /*kb:cons*/ k.table_name, k.constraint_name, k.constraint_type, k.status, cc.column_name, r.owner, r.table_name, rc.column_name \
         FROM all_constraints k JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
         LEFT JOIN all_constraints r ON k.constraint_type = 'R' AND r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
         LEFT JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name AND rc.position = cc.position \
         WHERE k.owner = :1 AND k.constraint_type IN ('P','U','R') ORDER BY k.table_name, k.constraint_name, cc.position",
        &b,
    )?;
    for r in &rows {
        if let Some(t) = tables.get_mut(&s(r, 0)) {
            let name = s(r, 1);
            let c = match t.constraints.iter().position(|c| c.name == name) {
                Some(i) => &mut t.constraints[i],
                None => {
                    t.constraints.push(ConFacts {
                        name,
                        kind: s(r, 2),
                        status: s(r, 3),
                        columns: vec![],
                        ref_table: opt(r, 6).map(|n| format!("{}.{}", s(r, 5), n)),
                        ref_columns: vec![],
                    });
                    t.constraints.last_mut().unwrap()
                }
            };
            c.columns.push(s(r, 4));
            if let Some(rc) = opt(r, 7) {
                c.ref_columns.push(rc);
            }
        }
    }
    let (_, rows) = collect(
        db,
        "SELECT /*kb:idx*/ i.table_name, i.index_name, i.uniqueness, i.index_type, i.status, ic.column_name \
         FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
         WHERE i.table_owner = :1 ORDER BY i.table_name, i.index_name, ic.column_position",
        &b,
    )?;
    for r in &rows {
        if let Some(t) = tables.get_mut(&s(r, 0)) {
            let name = s(r, 1);
            match t.indexes.iter().position(|i| i.name == name) {
                Some(i) => t.indexes[i].columns.push(s(r, 5)),
                None => t.indexes.push(IdxFacts {
                    name,
                    unique: s(r, 2) == "UNIQUE",
                    index_type: s(r, 3),
                    status: s(r, 4),
                    columns: vec![s(r, 5)],
                }),
            }
        }
    }

    // ---- triggers, sequences, synonyms
    let (_, rows) = collect(
        db,
        "SELECT /*kb:trg*/ trigger_name, table_name, trigger_type, triggering_event, status FROM all_triggers WHERE owner = :1",
        &b,
    )?;
    let triggers: Vec<TriggerFacts> = rows
        .iter()
        .map(|r| TriggerFacts {
            name: s(r, 0),
            table: opt(r, 1),
            trigger_type: s(r, 2),
            event: s(r, 3),
            status: s(r, 4),
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT /*kb:seq*/ sequence_name, min_value, max_value, increment_by, cycle_flag, cache_size, last_number FROM all_sequences WHERE sequence_owner = :1",
        &b,
    )?;
    let sequences: Vec<SeqFacts> = rows
        .iter()
        .map(|r| SeqFacts {
            name: s(r, 0),
            min: s(r, 1),
            max: s(r, 2),
            increment: s(r, 3),
            cycle: s(r, 4) == "Y",
            cache: s(r, 5),
            last: s(r, 6),
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT /*kb:syn*/ synonym_name, table_owner, table_name, db_link FROM all_synonyms WHERE owner = :1",
        &b,
    )?;
    let synonyms: Vec<SynFacts> = rows
        .iter()
        .map(|r| SynFacts {
            name: s(r, 0),
            target_owner: s(r, 1),
            target: s(r, 2),
            db_link: opt(r, 3),
        })
        .collect();

    // ---- optional sources
    let mut grants: BTreeMap<String, Vec<Grant>> = BTreeMap::new();
    match collect(
        db,
        "SELECT /*kb:grants*/ table_name, grantee, privilege FROM all_tab_privs WHERE table_schema = :1 ORDER BY table_name, grantee, privilege",
        &b,
    ) {
        Ok((_, rows)) => {
            for r in &rows {
                grants.entry(s(r, 0)).or_default().push(Grant {
                    object: s(r, 0),
                    grantee: s(r, 1),
                    privilege: s(r, 2),
                });
            }
        }
        Err(e) => warn("grants", format!("Grants could not be read ({e}); access rights are not in the knowledge base.")),
    }
    let mut jobs: Vec<JobFacts> = Vec::new();
    match collect(
        db,
        "SELECT /*kb:jobs*/ job_name, job_type, job_action, enabled FROM all_scheduler_jobs WHERE owner = :1",
        &b,
    ) {
        Ok((_, rows)) => {
            jobs = rows
                .iter()
                .map(|r| JobFacts {
                    name: s(r, 0),
                    job_type: s(r, 1),
                    action: s(r, 2),
                    enabled: s(r, 3).eq_ignore_ascii_case("TRUE"),
                })
                .collect()
        }
        Err(e) => warn("jobs", format!("Scheduler jobs could not be read ({e}); jobs that call this schema are not shown.")),
    }
    let mut deps: Vec<Row> = Vec::new();
    match collect(
        db,
        "SELECT /*kb:deps*/ name, type, referenced_owner, referenced_name, referenced_type FROM all_dependencies \
         WHERE owner = :1 AND referenced_owner NOT IN ('SYS','PUBLIC') AND referenced_type <> 'NON-EXISTENT'",
        &b,
    ) {
        Ok((_, rows)) => deps = rows,
        Err(e) => warn("dependencies", format!("ALL_DEPENDENCIES could not be read ({e}); links between objects are incomplete.")),
    }
    let mut ext: Vec<Row> = Vec::new();
    if let Ok((_, rows)) = collect(
        db,
        "SELECT /*kb:extdeps*/ owner, name, type, referenced_name, referenced_type FROM all_dependencies WHERE referenced_owner = :1 AND owner <> :1",
        &b,
    ) {
        ext = rows;
    }
    let packages = packages::analyze_schema(db, owner)?;

    // ---- graph with evidence
    let mut g = Builder::default();
    for o in &objects {
        g.node(&o.kind, owner, &o.name);
    }
    for r in &deps {
        let from = g.node(&s(r, 1), owner, &s(r, 0));
        let to = g.node(&s(r, 4), &s(r, 2), &s(r, 3));
        g.link(&from, &to, "uses", "dictionary", "ALL_DEPENDENCIES".into());
    }
    for r in &ext {
        let from = g.node(&s(r, 2), &s(r, 0), &s(r, 1));
        let to = g.node(&s(r, 4), owner, &s(r, 3));
        g.link(
            &from,
            &to,
            "uses",
            "dictionary",
            format!("ALL_DEPENDENCIES (used from schema {})", s(r, 0)),
        );
    }
    for t in tables.values() {
        let tid = g.node(&t.kind, owner, &t.name);
        for c in &t.constraints {
            if let (true, Some(rt)) = (c.kind == "R", &c.ref_table) {
                let (ro, rn) = rt.split_once('.').unwrap_or((owner, rt));
                let to = g.node("TABLE", ro, rn);
                let cols: Vec<String> = c
                    .columns
                    .iter()
                    .zip(
                        c.ref_columns
                            .iter()
                            .chain(std::iter::repeat(&String::new())),
                    )
                    .map(|(a, b)| format!("{}.{} -> {}.{}", t.name, a, rn, b))
                    .collect();
                g.link(
                    &tid,
                    &to,
                    "fk",
                    "dictionary",
                    format!("{} ({})", cols.join(", "), c.name),
                );
            }
        }
        for i in &t.indexes {
            let iid = g.node("INDEX", owner, &i.name);
            g.link(
                &iid,
                &tid,
                "indexes",
                "dictionary",
                format!(
                    "{}({}){}",
                    i.name,
                    i.columns.join(", "),
                    if i.unique { " UNIQUE" } else { "" }
                ),
            );
        }
    }
    for t in &triggers {
        if let Some(tab) = &t.table {
            let tr = g.node("TRIGGER", owner, &t.name);
            let tb = g.node("TABLE", owner, tab);
            g.link(
                &tr,
                &tb,
                "uses",
                "dictionary",
                format!("{} {}", t.trigger_type, t.event),
            );
        }
    }
    for sy in &synonyms {
        let from = g.node("SYNONYM", owner, &sy.name);
        let to = g.node("TABLE", &sy.target_owner, &sy.target);
        let via = sy
            .db_link
            .as_deref()
            .map(|l| format!(" @{l}"))
            .unwrap_or_default();
        g.link(
            &from,
            &to,
            "uses",
            "dictionary",
            format!("synonym for {}.{}{via}", sy.target_owner, sy.target),
        );
    }
    for j in &jobs {
        let jid = g.node("JOB", owner, &j.name);
        if j.job_type.eq_ignore_ascii_case("STORED_PROCEDURE")
            || j.job_type.eq_ignore_ascii_case("PLSQL_BLOCK")
        {
            let action = j.action.to_uppercase();
            for o in objects
                .iter()
                .filter(|o| matches!(o.kind.as_str(), "PROCEDURE" | "FUNCTION" | "PACKAGE"))
            {
                let hit = action
                    .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '#')))
                    .any(|w| w == o.name);
                if hit {
                    let to = g.node(&o.kind, owner, &o.name);
                    g.link(
                        &jid,
                        &to,
                        "runs",
                        "dictionary",
                        format!("job action: {}", j.action.trim()),
                    );
                }
            }
        }
    }
    let table_kind: BTreeMap<&str, &str> = tables
        .values()
        .map(|t| (t.name.as_str(), t.kind.as_str()))
        .collect();
    for p in &packages.packages {
        let pid = g.node("PACKAGE", owner, &p.name);
        let mut per_table: BTreeMap<(String, bool), BTreeMap<String, BTreeSet<usize>>> =
            BTreeMap::new();
        for m in &p.members {
            for tl in &m.table_lines {
                per_table
                    .entry((tl.table.clone(), tl.write))
                    .or_default()
                    .entry(m.name.clone())
                    .or_default()
                    .insert(tl.line);
            }
            for c in &m.calls {
                let to = g.node("PACKAGE", owner, &c.package);
                g.link(
                    &pid,
                    &to,
                    "calls",
                    "scan",
                    format!("{}.{} -> {}.{}", p.name, m.name, c.package, c.member),
                );
            }
        }
        for ((table, write), members) in per_table {
            let kind = table_kind.get(table.as_str()).copied().unwrap_or("TABLE");
            let to = g.node(kind, owner, &table);
            let detail = members
                .iter()
                .map(|(m, ls)| {
                    format!(
                        "{}.{} (lines {})",
                        p.name,
                        m,
                        ls.iter()
                            .map(usize::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            g.link(
                &pid,
                &to,
                if write { "writes" } else { "reads" },
                "scan",
                detail,
            );
        }
    }

    // ---- coverage
    let wrapped: Vec<&str> = packages
        .packages
        .iter()
        .filter(|p| p.wrapped)
        .map(|p| p.name.as_str())
        .collect();
    let dyn_sites: Vec<(String, usize)> = packages
        .packages
        .iter()
        .flat_map(|p| {
            p.members
                .iter()
                .filter(|m| !m.dynamic_sql_lines.is_empty())
                .map(move |m| (format!("{}.{}", p.name, m.name), m.dynamic_sql_lines.len()))
        })
        .collect();
    let dyn_total: usize = dyn_sites.iter().map(|(_, n)| n).sum();
    if !wrapped.is_empty() {
        warn("packages", format!("{} wrapped package(s) cannot be read, so what they call, read or write is unknown: {}.", wrapped.len(), wrapped.join(", ")));
    }
    if dyn_total > 0 {
        let top: Vec<String> = dyn_sites
            .iter()
            .take(10)
            .map(|(m, n)| format!("{m} ({n})"))
            .collect();
        warn("dynamic SQL", format!("{dyn_total} dynamic SQL site(s) in {} member(s): the tables they touch cannot be known statically: {}{}.", dyn_sites.len(), top.join(", "), if dyn_sites.len() > 10 { ", …" } else { "" }));
    }
    let invalid: Vec<String> = objects
        .iter()
        .filter(|o| o.status != "VALID")
        .map(|o| format!("{} {}", o.kind.to_lowercase(), o.name))
        .collect();
    if !invalid.is_empty() {
        warn(
            "invalid objects",
            format!(
                "{} object(s) are INVALID, so their dependencies may be stale: {}.",
                invalid.len(),
                invalid.join(", ")
            ),
        );
    }
    let links: Vec<&str> = synonyms
        .iter()
        .filter(|x| x.db_link.is_some())
        .map(|x| x.name.as_str())
        .collect();
    if !links.is_empty() {
        warn(
            "database links",
            format!(
                "{} synonym(s) point over a database link; the remote side is not followed: {}.",
                links.len(),
                links.join(", ")
            ),
        );
    }
    gaps.push(Gap {
        severity: "info".into(),
        area: "package calls".into(),
        message: "Reads, writes and calls inside packages come from a text scan (heuristic). PL/Scope is not used yet.".into(),
    });
    gaps.push(Gap {
        severity: "info".into(),
        area: "callers".into(),
        message: format!("{} object(s) in other schemas use this schema (shown). Applications and external callers are not visible to the database.", ext.len()),
    });
    let members: usize = packages.packages.iter().map(|p| p.members.len()).sum();
    let coverage = Coverage {
        gaps,
        packages: packages.packages.len(),
        packages_scanned: packages.packages.len() - wrapped.len(),
        packages_wrapped: wrapped.len(),
        members,
        dynamic_sql_sites: dyn_total,
    };

    let edges: Vec<GEdge> = g
        .edges
        .into_iter()
        .map(|((from, to), kind)| GEdge { from, to, kind })
        .collect();
    let mut evidence = g.evidence;
    evidence.sort_by(|a, b| {
        (&a.from, &a.to, &a.kind, &a.detail).cmp(&(&b.from, &b.to, &b.kind, &b.detail))
    });
    Ok(Knowledge {
        owner: owner.to_string(),
        taken_at,
        tables: tables.into_values().collect(),
        triggers,
        sequences,
        synonyms,
        objects,
        jobs,
        grants,
        packages,
        nodes: g.nodes.into_values().collect(),
        edges,
        evidence,
        coverage,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};
    use crate::graph::{Direction, Focus};

    fn t(cols: &[&str], rows: Vec<Vec<Option<&str>>>) -> FakeTable {
        FakeTable::new(cols, rows)
    }
    fn some(v: &str) -> Option<&str> {
        Some(v)
    }

    pub(crate) fn db(with_grants: bool) -> FakeDb {
        let mut d = FakeDb::new()
            .with_table("kb:objs", t(&["n", "t", "s", "d"], vec![
                vec![some("EMPLOYEES"), some("TABLE"), some("VALID"), some("2026-01-01 10:00")],
                vec![some("DEPARTMENTS"), some("TABLE"), some("VALID"), None],
                vec![some("EMP_V"), some("VIEW"), some("INVALID"), None],
                vec![some("EMP_BI"), some("TRIGGER"), some("VALID"), None],
                vec![some("EMP_SEQ"), some("SEQUENCE"), some("VALID"), None],
                vec![some("EMP"), some("SYNONYM"), some("VALID"), None],
                vec![some("EMP_PKG"), some("PACKAGE"), some("VALID"), None],
                vec![some("SECRET_PKG"), some("PACKAGE"), some("VALID"), None],
            ]))
            .with_table("kb:tables", t(&["n", "c", "r", "a"], vec![
                vec![some("EMPLOYEES"), some("People"), some("107"), some("2026-02-01 00:00")],
                vec![some("DEPARTMENTS"), None, None, None],
            ]))
            .with_table("kb:cols", t(&["t", "c", "dt", "dl", "cl", "cu", "p", "sc", "n", "cm"], vec![
                vec![some("EMPLOYEES"), some("EMPLOYEE_ID"), some("NUMBER"), some("22"), some("0"), some("B"), some("6"), some("0"), some("N"), some("PK")],
                vec![some("EMPLOYEES"), some("DEPT_ID"), some("NUMBER"), some("22"), some("0"), some("B"), some("4"), some("0"), some("Y"), None],
                vec![some("DEPARTMENTS"), some("DEPT_ID"), some("NUMBER"), some("22"), some("0"), some("B"), some("4"), some("0"), some("N"), None],
            ]))
            .with_table("kb:cons", t(&["t", "n", "ty", "st", "c", "ro", "rt", "rc"], vec![
                vec![some("EMPLOYEES"), some("EMP_PK"), some("P"), some("ENABLED"), some("EMPLOYEE_ID"), None, None, None],
                vec![some("EMPLOYEES"), some("EMP_DEPT_FK"), some("R"), some("ENABLED"), some("DEPT_ID"), some("HR"), some("DEPARTMENTS"), some("DEPT_ID")],
            ]))
            .with_table("kb:idx", t(&["t", "i", "u", "ty", "st", "c"], vec![
                vec![some("EMPLOYEES"), some("EMP_DEPT_IX"), some("NONUNIQUE"), some("NORMAL"), some("VALID"), some("DEPT_ID")],
            ]))
            .with_table("kb:trg", t(&["n", "t", "ty", "e", "s"], vec![vec![some("EMP_BI"), some("EMPLOYEES"), some("BEFORE EACH ROW"), some("INSERT"), some("ENABLED")]]))
            .with_table("kb:seq", t(&["n", "mi", "ma", "i", "c", "ca", "l"], vec![vec![some("EMP_SEQ"), some("1"), some("999"), some("1"), some("N"), some("20"), some("207")]]))
            .with_table("kb:syn", t(&["n", "o", "t", "l"], vec![vec![some("EMP"), some("HR"), some("EMPLOYEES"), None], vec![some("REMOTE_T"), some("X"), some("Y"), some("LNK")]]))
            .with_table("kb:jobs", t(&["n", "t", "a", "e"], vec![vec![some("NIGHTLY"), some("STORED_PROCEDURE"), some("HR.EMP_PKG.RAISE_ALL"), some("TRUE")]]))
            .with_table("kb:deps", t(&["n", "t", "ro", "rn", "rt"], vec![
                vec![some("EMP_V"), some("VIEW"), some("HR"), some("EMPLOYEES"), some("TABLE")],
                vec![some("EMP_BI"), some("TRIGGER"), some("HR"), some("EMP_SEQ"), some("SEQUENCE")],
                vec![some("EMP_PKG"), some("PACKAGE BODY"), some("HR"), some("EMPLOYEES"), some("TABLE")],
            ]))
            .with_table("kb:extdeps", t(&["o", "n", "t", "rn", "rt"], vec![vec![some("REPORTS"), some("MONTHLY_V"), some("VIEW"), some("EMPLOYEES"), some("TABLE")]]))
            .with_table("pkg:objs", t(&["n", "t", "s"], vec![
                vec![some("EMP_PKG"), some("PACKAGE"), some("VALID")], vec![some("EMP_PKG"), some("PACKAGE BODY"), some("VALID")],
                vec![some("SECRET_PKG"), some("PACKAGE"), some("VALID")], vec![some("SECRET_PKG"), some("PACKAGE BODY"), some("VALID")],
            ]))
            .with_table("pkg:procs", t(&["a", "b", "c"], vec![vec![some("EMP_PKG"), some("RAISE_ALL"), None]]))
            .with_table("pkg:args", t(&["a", "b", "c", "d", "e", "f", "g"], vec![]))
            .with_table("pkg:tables", t(&["n"], vec![vec![some("EMPLOYEES")], vec![some("DEPARTMENTS")], vec![some("EMP_V")]]))
            .with_table("pkg:bodies", t(&["n", "t"], vec![
                vec![some("EMP_PKG"), some("PACKAGE BODY emp_pkg IS\n  PROCEDURE raise_all IS BEGIN\n    UPDATE employees SET x = 1;\n    EXECUTE IMMEDIATE 'ANALYZE';\n  END;\nEND;\n")],
                vec![some("SECRET_PKG"), some("package body secret_pkg wrapped\na000\n")],
            ]));
        d = if with_grants {
            d.with_table(
                "kb:grants",
                t(
                    &["o", "g", "p"],
                    vec![vec![some("EMPLOYEES"), some("APP_RO"), some("SELECT")]],
                ),
            )
        } else {
            d.with_error("kb:grants", "ORA-01031: insufficient privileges")
        };
        d
    }

    fn id(kind: &str, name: &str) -> String {
        format!("{kind}:HR.{name}")
    }

    #[test]
    fn facts_are_collected_with_columns_keys_and_indexes() {
        let k = collect_schema(&db(true), "HR", 1).unwrap();
        let e = k.table("EMPLOYEES").unwrap();
        assert_eq!(e.comment.as_deref(), Some("People"));
        assert_eq!(e.num_rows, Some(107));
        assert_eq!(e.columns.len(), 2);
        assert_eq!(e.columns[0].data_type, "NUMBER(6)");
        assert!(!e.columns[0].nullable);
        let fk = e.constraints.iter().find(|c| c.kind == "R").unwrap();
        assert_eq!(
            (fk.ref_table.as_deref(), fk.ref_columns.clone()),
            (Some("HR.DEPARTMENTS"), vec!["DEPT_ID".to_string()])
        );
        assert_eq!(e.indexes[0].columns, vec!["DEPT_ID"]);
        assert_eq!(k.grants["EMPLOYEES"][0].grantee, "APP_RO");
        let s = k.summary();
        assert_eq!(
            (s.tables, s.views, s.triggers, s.indexes, s.sequences),
            (2, 1, 1, 1, 1)
        );
    }

    #[test]
    fn links_carry_evidence_and_the_most_specific_kind_wins() {
        let k = collect_schema(&db(true), "HR", 1).unwrap();
        let kind = |a: &str, b: &str| {
            k.edges
                .iter()
                .find(|e| e.from == a && e.to == b)
                .map(|e| e.kind.clone())
        };
        assert_eq!(
            kind(&id("PACKAGE", "EMP_PKG"), &id("TABLE", "EMPLOYEES")).as_deref(),
            Some("writes"),
            "scan upgrades the dictionary 'uses' edge"
        );
        assert_eq!(
            kind(&id("TABLE", "EMPLOYEES"), &id("TABLE", "DEPARTMENTS")).as_deref(),
            Some("fk")
        );
        assert_eq!(
            kind(&id("INDEX", "EMP_DEPT_IX"), &id("TABLE", "EMPLOYEES")).as_deref(),
            Some("indexes")
        );
        assert_eq!(
            kind(&id("TRIGGER", "EMP_BI"), &id("TABLE", "EMPLOYEES")).as_deref(),
            Some("uses")
        );
        assert_eq!(
            kind(&id("JOB", "NIGHTLY"), &id("PACKAGE", "EMP_PKG")).as_deref(),
            Some("runs")
        );
        assert_eq!(
            kind(&id("SYNONYM", "EMP"), &id("TABLE", "EMPLOYEES")).as_deref(),
            Some("uses")
        );
        let ev = k.evidence_between(&id("PACKAGE", "EMP_PKG"), &id("TABLE", "EMPLOYEES"));
        assert!(ev
            .iter()
            .any(|e| e.source == "dictionary" && e.confidence == "exact"));
        let scan = ev.iter().find(|e| e.source == "scan").unwrap();
        assert_eq!(scan.confidence, "heuristic");
        assert!(
            scan.detail.contains("EMP_PKG.RAISE_ALL (lines 3)"),
            "{}",
            scan.detail
        );
        let fk = k.evidence_between(&id("TABLE", "EMPLOYEES"), &id("TABLE", "DEPARTMENTS"));
        assert!(
            fk[0]
                .detail
                .contains("EMPLOYEES.DEPT_ID -> DEPARTMENTS.DEPT_ID (EMP_DEPT_FK)"),
            "{}",
            fk[0].detail
        );
    }

    #[test]
    fn other_schemas_that_use_us_are_in_the_graph() {
        let k = collect_schema(&db(true), "HR", 1).unwrap();
        assert!(k.nodes.iter().any(|n| n.id == "VIEW:REPORTS.MONTHLY_V"));
        let s = k
            .graph()
            .focus(&Focus {
                id: id("TABLE", "EMPLOYEES"),
                direction: Direction::UsedBy,
                hops: 2,
                kinds: None,
                max_nodes: None,
            })
            .unwrap();
        let names: Vec<&str> = s.nodes.iter().map(|n| n.node.name.as_str()).collect();
        for want in [
            "EMP_PKG",
            "EMP_V",
            "EMP_BI",
            "EMP_DEPT_IX",
            "MONTHLY_V",
            "EMP",
            "NIGHTLY",
        ] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
    }

    #[test]
    fn coverage_lists_what_cannot_be_seen_instead_of_hiding_it() {
        let k = collect_schema(&db(false), "HR", 1).unwrap();
        let text: Vec<String> = k
            .coverage
            .gaps
            .iter()
            .map(|g| format!("{}: {}", g.area, g.message))
            .collect();
        let all = text.join("\n");
        assert!(
            all.contains("wrapped") && all.contains("SECRET_PKG"),
            "{all}"
        );
        assert!(
            all.contains("dynamic SQL") && all.contains("EMP_PKG.RAISE_ALL"),
            "{all}"
        );
        assert!(all.contains("INVALID") && all.contains("EMP_V"), "{all}");
        assert!(
            all.contains("database link") && all.contains("REMOTE_T"),
            "{all}"
        );
        assert!(all.contains("Grants could not be read"), "{all}");
        assert_eq!(
            (
                k.coverage.packages,
                k.coverage.packages_wrapped,
                k.coverage.dynamic_sql_sites
            ),
            (2, 1, 1)
        );
    }

    #[test]
    fn snapshot_round_trips_through_json_and_rebuilds_the_graph() {
        let k = collect_schema(&db(true), "HR", 42).unwrap();
        let json = serde_json::to_string(&k).unwrap();
        let back: Knowledge = serde_json::from_str(&json).unwrap();
        assert_eq!(back.taken_at, 42);
        assert_eq!(back.edges, k.edges);
        assert_eq!(back.graph().info().edges, k.graph().info().edges);
    }
}
