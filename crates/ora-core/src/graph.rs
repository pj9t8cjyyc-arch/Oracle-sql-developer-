//! Linkage graph: what uses what. Nodes are tables, views, packages, procedures, functions,
//! triggers, sequences, types and synonyms; an edge `a -> b` means "a uses b" (a dependency from
//! `ALL_DEPENDENCIES`, or a foreign key from child table to parent table).
//!
//! The whole schema graph is loaded once (two queries) and traversed in memory, so changing the
//! focus, direction or hop limit never touches the database.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::db::{collect, Database, Row};
use crate::Result;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GNode {
    /// `KIND:OWNER.NAME`
    pub id: String,
    pub owner: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct GEdge {
    pub from: String,
    pub to: String,
    /// `uses` (ALL_DEPENDENCIES) or `fk` (foreign key, child -> parent).
    pub kind: String,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub owner: String,
    nodes: BTreeMap<String, GNode>,
    edges: BTreeSet<GEdge>,
    out: HashMap<String, Vec<usize>>,
    inc: HashMap<String, Vec<usize>>,
    list: Vec<GEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphInfo {
    pub owner: String,
    pub nodes: usize,
    pub edges: usize,
    pub kinds: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// What the focus depends on.
    Uses,
    /// What depends on the focus (what breaks if it changes).
    UsedBy,
    Both,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Focus {
    pub id: String,
    pub direction: Direction,
    pub hops: u32,
    /// Only these kinds are shown and followed (`None` = all).
    pub kinds: Option<Vec<String>>,
    pub max_nodes: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SNode {
    #[serde(flatten)]
    pub node: GNode,
    /// 0 = focus, positive = distance on the "uses" side, negative = on the "used by" side.
    pub depth: i32,
    /// Neighbours beyond the hop limit that are not shown.
    pub more: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Subgraph {
    pub focus: String,
    pub nodes: Vec<SNode>,
    pub edges: Vec<GEdge>,
    pub truncated: bool,
}

const MAX_HOPS: u32 = 8;
const DEFAULT_MAX_NODES: usize = 150;

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}

fn norm_kind(k: &str) -> String {
    match k {
        "PACKAGE BODY" => "PACKAGE",
        "TYPE BODY" => "TYPE",
        o => o,
    }
    .to_string()
}

pub fn node_id(kind: &str, owner: &str, name: &str) -> String {
    format!("{}:{}.{}", norm_kind(kind), owner, name)
}

impl Graph {
    pub fn new(owner: &str) -> Self {
        Self {
            owner: owner.to_string(),
            ..Default::default()
        }
    }

    pub fn add_node(&mut self, kind: &str, owner: &str, name: &str) -> String {
        let kind = norm_kind(kind);
        let id = node_id(&kind, owner, name);
        self.nodes.entry(id.clone()).or_insert_with(|| GNode {
            id: id.clone(),
            owner: owner.to_string(),
            name: name.to_string(),
            kind,
        });
        id
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: &str) {
        if from != to {
            self.edges.insert(GEdge {
                from: from.to_string(),
                to: to.to_string(),
                kind: kind.to_string(),
            });
        }
    }

    /// Builds the adjacency lists; call once after loading.
    pub fn finish(&mut self) {
        self.list = self.edges.iter().cloned().collect();
        self.out.clear();
        self.inc.clear();
        for (i, e) in self.list.iter().enumerate() {
            self.out.entry(e.from.clone()).or_default().push(i);
            self.inc.entry(e.to.clone()).or_default().push(i);
        }
    }

    pub fn info(&self) -> GraphInfo {
        let mut kinds = BTreeMap::new();
        for n in self.nodes.values() {
            *kinds.entry(n.kind.clone()).or_default() += 1;
        }
        GraphInfo {
            owner: self.owner.clone(),
            nodes: self.nodes.len(),
            edges: self.list.len(),
            kinds,
        }
    }

    pub fn node(&self, id: &str) -> Option<&GNode> {
        self.nodes.get(id)
    }

    /// Name search for the focus picker: prefix matches first, then substring matches.
    pub fn search(&self, query: &str, limit: usize) -> Vec<GNode> {
        let q = query.trim().to_uppercase();
        let mut pre = Vec::new();
        let mut sub = Vec::new();
        for n in self.nodes.values() {
            let name = n.name.to_uppercase();
            if q.is_empty() || name.starts_with(&q) {
                pre.push(n.clone());
            } else if name.contains(&q) {
                sub.push(n.clone());
            }
        }
        pre.sort_by(|a, b| a.name.cmp(&b.name));
        sub.sort_by(|a, b| a.name.cmp(&b.name));
        pre.extend(sub);
        pre.truncate(limit);
        pre
    }

    /// Breadth-first neighbourhood of one object.
    pub fn focus(&self, f: &Focus) -> Option<Subgraph> {
        self.nodes.get(&f.id)?;
        let hops = f.hops.clamp(1, MAX_HOPS) as i32;
        let max = f.max_nodes.unwrap_or(DEFAULT_MAX_NODES).max(1);
        let allowed = |id: &str| match (&f.kinds, self.nodes.get(id)) {
            (Some(k), Some(n)) => k.contains(&n.kind),
            (None, Some(_)) => true,
            _ => false,
        };
        let mut depth: HashMap<String, i32> = HashMap::new();
        depth.insert(f.id.clone(), 0);
        let mut truncated = false;
        for (sign, adj) in [(1, &self.out), (-1, &self.inc)] {
            let wanted = match f.direction {
                Direction::Uses => sign == 1,
                Direction::UsedBy => sign == -1,
                Direction::Both => true,
            };
            if !wanted {
                continue;
            }
            let mut seen: HashSet<String> = HashSet::from([f.id.clone()]);
            let mut q: VecDeque<(String, i32)> = VecDeque::from([(f.id.clone(), 0)]);
            while let Some((id, d)) = q.pop_front() {
                if d >= hops {
                    continue;
                }
                for &ei in adj.get(&id).map(Vec::as_slice).unwrap_or(&[]) {
                    let e = &self.list[ei];
                    let next = if sign == 1 { &e.to } else { &e.from };
                    if seen.contains(next) || !allowed(next) {
                        continue;
                    }
                    if depth.len() >= max {
                        truncated = true;
                        continue;
                    }
                    seen.insert(next.clone());
                    // A node reachable from both sides keeps the side it was found on first.
                    depth.entry(next.clone()).or_insert(sign * (d + 1));
                    q.push_back((next.clone(), d + 1));
                }
            }
        }
        let shown: HashSet<&String> = depth.keys().collect();
        let mut edges: Vec<GEdge> = self
            .list
            .iter()
            .filter(|e| shown.contains(&e.from) && shown.contains(&e.to))
            .cloned()
            .collect();
        edges.sort();
        let more = |id: &String, d: i32| -> usize {
            if d.abs() != hops && !(truncated && d != 0) {
                return 0;
            }
            let mut extra: HashSet<&String> = HashSet::new();
            let want_out = d >= 0 && f.direction != Direction::UsedBy;
            let want_in = d <= 0 && f.direction != Direction::Uses;
            if want_out {
                for &ei in self.out.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                    let t = &self.list[ei].to;
                    if !shown.contains(t) && allowed(t) {
                        extra.insert(t);
                    }
                }
            }
            if want_in {
                for &ei in self.inc.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                    let t = &self.list[ei].from;
                    if !shown.contains(t) && allowed(t) {
                        extra.insert(t);
                    }
                }
            }
            extra.len()
        };
        let mut nodes: Vec<SNode> = depth
            .iter()
            .map(|(id, &d)| SNode {
                node: self.nodes[id].clone(),
                depth: d,
                more: more(id, d),
            })
            .collect();
        nodes.sort_by(|a, b| {
            (a.depth, &a.node.kind, &a.node.name).cmp(&(b.depth, &b.node.kind, &b.node.name))
        });
        Some(Subgraph {
            focus: f.id.clone(),
            nodes,
            edges,
            truncated,
        })
    }
}

/// Loads the dependency graph of one schema. Dependencies are those *of objects in the schema*;
/// objects in other schemas appear when this schema uses them.
pub fn load(db: &dyn Database, owner: &str) -> Result<Graph> {
    let b = [owner.to_string()];
    let mut g = Graph::new(owner);
    let (_, objs) = collect(
        db,
        "SELECT /*graph:objs*/ object_name, object_type FROM all_objects WHERE owner = :1 \
         AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','TRIGGER','PROCEDURE','FUNCTION','PACKAGE','TYPE','SYNONYM')",
        &b,
    )?;
    for r in &objs {
        g.add_node(&s(r, 1), owner, &s(r, 0));
    }
    let (_, deps) = collect(
        db,
        "SELECT /*graph:deps*/ name, type, referenced_owner, referenced_name, referenced_type FROM all_dependencies \
         WHERE owner = :1 AND referenced_owner NOT IN ('SYS','PUBLIC') AND referenced_type <> 'NON-EXISTENT' \
         AND type IN ('TABLE','VIEW','MATERIALIZED VIEW','TRIGGER','PROCEDURE','FUNCTION','PACKAGE','PACKAGE BODY','TYPE','TYPE BODY','SYNONYM')",
        &b,
    )?;
    for r in &deps {
        let from = g.add_node(&s(r, 1), owner, &s(r, 0));
        let to = g.add_node(&s(r, 4), &s(r, 2), &s(r, 3));
        g.add_edge(&from, &to, "uses");
    }
    let (_, fks) = collect(
        db,
        "SELECT /*graph:fks*/ c.table_name, p.owner, p.table_name FROM all_constraints c \
         JOIN all_constraints p ON p.owner = c.r_owner AND p.constraint_name = c.r_constraint_name \
         WHERE c.owner = :1 AND c.constraint_type = 'R'",
        &b,
    )?;
    for r in &fks {
        let from = g.add_node("TABLE", owner, &s(r, 0));
        let to = g.add_node("TABLE", &s(r, 1), &s(r, 2));
        g.add_edge(&from, &to, "fk");
    }
    g.finish();
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn sample() -> Graph {
        let mut g = Graph::new("HR");
        let emp = g.add_node("TABLE", "HR", "EMPLOYEES");
        let dep = g.add_node("TABLE", "HR", "DEPARTMENTS");
        let loc = g.add_node("TABLE", "HR", "LOCATIONS");
        let pk = g.add_node("PACKAGE", "HR", "EMP_PKG");
        let tr = g.add_node("TRIGGER", "HR", "EMP_BI");
        let sq = g.add_node("SEQUENCE", "HR", "EMP_SEQ");
        let vw = g.add_node("VIEW", "HR", "EMP_V");
        let rep = g.add_node("PACKAGE", "HR", "REPORT_PKG");
        g.add_edge(&emp, &dep, "fk");
        g.add_edge(&dep, &loc, "fk");
        g.add_edge(&pk, &emp, "uses");
        g.add_edge(&tr, &emp, "uses");
        g.add_edge(&tr, &sq, "uses");
        g.add_edge(&vw, &emp, "uses");
        g.add_edge(&rep, &vw, "uses");
        g.add_edge(&rep, &pk, "uses");
        g.finish();
        g
    }

    fn f(id: &str, d: Direction, hops: u32) -> Focus {
        Focus {
            id: id.into(),
            direction: d,
            hops,
            kinds: None,
            max_nodes: None,
        }
    }
    fn names(s: &Subgraph) -> Vec<String> {
        s.nodes
            .iter()
            .map(|n| format!("{}@{}", n.node.name, n.depth))
            .collect()
    }

    #[test]
    fn what_breaks_if_i_change_a_table() {
        let g = sample();
        let s = g
            .focus(&f("TABLE:HR.EMPLOYEES", Direction::UsedBy, 1))
            .unwrap();
        assert_eq!(
            names(&s),
            vec!["EMP_PKG@-1", "EMP_BI@-1", "EMP_V@-1", "EMPLOYEES@0"]
        );
        let s2 = g
            .focus(&f("TABLE:HR.EMPLOYEES", Direction::UsedBy, 2))
            .unwrap();
        assert!(names(&s2).contains(&"REPORT_PKG@-2".to_string()));
        assert!(s2
            .edges
            .iter()
            .any(|e| e.from.ends_with("REPORT_PKG") && e.to.ends_with("EMP_PKG")));
    }

    #[test]
    fn what_a_package_needs_and_hop_limits() {
        let g = sample();
        let s = g
            .focus(&f("PACKAGE:HR.REPORT_PKG", Direction::Uses, 1))
            .unwrap();
        assert_eq!(names(&s), vec!["REPORT_PKG@0", "EMP_PKG@1", "EMP_V@1"]);
        // Frontier nodes say how much is hidden beyond the limit.
        assert_eq!(
            s.nodes
                .iter()
                .find(|n| n.node.name == "EMP_V")
                .unwrap()
                .more,
            1
        );
        let all = g
            .focus(&f("PACKAGE:HR.REPORT_PKG", Direction::Uses, 8))
            .unwrap();
        assert!(names(&all).contains(&"LOCATIONS@4".to_string()));
        assert!(all.nodes.iter().all(|n| n.more == 0));
    }

    #[test]
    fn both_directions_and_kind_filter() {
        let g = sample();
        let s = g
            .focus(&f("TABLE:HR.EMPLOYEES", Direction::Both, 1))
            .unwrap();
        let n = names(&s);
        assert!(n.contains(&"DEPARTMENTS@1".to_string()) && n.contains(&"EMP_PKG@-1".to_string()));
        let mut only = f("TABLE:HR.EMPLOYEES", Direction::Both, 1);
        only.kinds = Some(vec!["TABLE".into()]);
        let t = g.focus(&only).unwrap();
        assert_eq!(names(&t), vec!["EMPLOYEES@0", "DEPARTMENTS@1"]);
    }

    #[test]
    fn node_cap_truncates_and_reports_it() {
        let g = sample();
        let mut c = f("TABLE:HR.EMPLOYEES", Direction::Both, 3);
        c.max_nodes = Some(3);
        let s = g.focus(&c).unwrap();
        assert_eq!(s.nodes.len(), 3);
        assert!(s.truncated);
        assert!(g.focus(&f("TABLE:HR.NOPE", Direction::Both, 1)).is_none());
    }

    #[test]
    fn cycles_terminate() {
        let mut g = Graph::new("HR");
        let a = g.add_node("PACKAGE", "HR", "A");
        let b = g.add_node("PACKAGE", "HR", "B");
        g.add_edge(&a, &b, "uses");
        g.add_edge(&b, &a, "uses");
        g.finish();
        let s = g.focus(&f(&a, Direction::Both, 8)).unwrap();
        assert_eq!(s.nodes.len(), 2);
    }

    #[test]
    fn search_prefers_prefix_matches() {
        let g = sample();
        let r: Vec<String> = g.search("emp", 10).into_iter().map(|n| n.name).collect();
        assert_eq!(&r[..4], &["EMPLOYEES", "EMP_BI", "EMP_PKG", "EMP_SEQ"]);
        assert!(g.search("pkg", 10).iter().any(|n| n.name == "REPORT_PKG"));
    }

    #[test]
    fn loader_merges_bodies_into_packages_and_drops_self_edges() {
        let db = FakeDb::new()
            .with_table(
                "graph:objs",
                FakeTable::new(
                    &["n", "t"],
                    vec![
                        vec![Some("P"), Some("PACKAGE")],
                        vec![Some("T"), Some("TABLE")],
                    ],
                ),
            )
            .with_table(
                "graph:deps",
                FakeTable::new(
                    &["n", "t", "ro", "rn", "rt"],
                    vec![
                        vec![
                            Some("P"),
                            Some("PACKAGE BODY"),
                            Some("HR"),
                            Some("P"),
                            Some("PACKAGE"),
                        ],
                        vec![
                            Some("P"),
                            Some("PACKAGE BODY"),
                            Some("HR"),
                            Some("T"),
                            Some("TABLE"),
                        ],
                        vec![
                            Some("P"),
                            Some("PACKAGE BODY"),
                            Some("HR"),
                            Some("T"),
                            Some("TABLE"),
                        ],
                    ],
                ),
            )
            .with_table(
                "graph:fks",
                FakeTable::new(
                    &["c", "po", "p"],
                    vec![vec![Some("T"), Some("HR"), Some("U")]],
                ),
            );
        let g = load(&db, "HR").unwrap();
        let i = g.info();
        assert_eq!(
            i.edges, 2,
            "body->spec dropped, duplicate dependency merged, plus one FK"
        );
        assert_eq!(i.kinds["PACKAGE"], 1);
        assert!(g.node("TABLE:HR.U").is_some());
    }
}
