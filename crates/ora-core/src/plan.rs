//! Explain plan: asks the optimizer for the *estimated* plan of a statement (without running it),
//! turns it into a tree with per-step cost shares, and adds plain-language findings found by fixed
//! rules (full scans of big tables, missing join conditions, implicit conversions, functions on
//! columns, stale statistics…).
//!
//! Everything here comes from the optimizer's estimates. It tells you where to look, not what the
//! statement will actually cost: confirm with a real run before changing anything.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::db::{collect, Database, Row};
use crate::Result;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PlanNode {
    pub id: u32,
    pub parent_id: Option<u32>,
    pub depth: u32,
    /// `TABLE ACCESS FULL`, `HASH JOIN`…
    pub operation: String,
    pub object_owner: Option<String>,
    pub object_name: Option<String>,
    pub object_alias: Option<String>,
    pub cost: Option<f64>,
    /// Cost of this step alone (its cost minus its children's).
    pub self_cost: f64,
    /// Share of the whole statement's cost that is this step alone, 0–100.
    pub cost_pct: f64,
    pub rows: Option<f64>,
    pub bytes: Option<f64>,
    pub seconds: Option<f64>,
    pub access: Option<String>,
    pub filter: Option<String>,
    pub children: u32,
    /// On the most expensive path from the root to a leaf.
    pub hot: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Finding {
    /// `high`, `warn` or `info`.
    pub severity: String,
    pub code: String,
    pub title: String,
    pub detail: String,
    /// Plan step ids this is about.
    pub steps: Vec<u32>,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plan {
    pub nodes: Vec<PlanNode>,
    pub total_cost: Option<f64>,
    /// The `Note` lines of the plan (dynamic statistics, baselines, adaptive plan…).
    pub notes: Vec<String>,
    /// The plan as Oracle prints it (`DBMS_XPLAN.DISPLAY`).
    pub text: String,
    pub findings: Vec<Finding>,
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.trim().is_empty())
}
fn num(r: &Row, i: usize) -> Option<f64> {
    r.get(i)
        .cloned()
        .flatten()
        .and_then(|v| v.trim().parse().ok())
}

/// Plans a statement. Nothing is executed; the plan rows this creates are removed again.
/// The text must be one SELECT/INSERT/UPDATE/DELETE/MERGE (the caller checks).
pub fn explain(db: &dyn Database, sql: &str) -> Result<Plan> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("ORAIDE{}", nanos % 1_000_000_000_000);
    let body = sql.trim().trim_end_matches(';').trim();
    db.run(
        &format!("EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR {body}"),
        &[],
        1,
    )?;
    let b = [id.clone()];
    // A private, older PLAN_TABLE may lack the newer columns: fall back to the classic ones.
    let rows = collect(
        db,
        "SELECT /*pl:rows_full*/ id, parent_id, operation, options, object_owner, object_name, object_alias, cost, cardinality, bytes, time, access_predicates, filter_predicates \
         FROM plan_table WHERE statement_id = :1 ORDER BY id",
        &b,
    )
    .or_else(|_| {
        collect(
            db,
            "SELECT /*pl:rows_old*/ id, parent_id, operation, options, object_owner, object_name, NULL, cost, cardinality, bytes, time, access_predicates, filter_predicates \
             FROM plan_table WHERE statement_id = :1 ORDER BY id",
            &b,
        )
    });
    let text = collect(
        db,
        "SELECT /*pl:text*/ plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY('PLAN_TABLE', :1, 'TYPICAL'))",
        &b,
    )
    .map(|(_, r)| r.iter().map(|x| s(x, 0)).collect::<Vec<_>>().join("\n"))
    .unwrap_or_default();
    let _ = db.run(
        &format!("DELETE FROM plan_table WHERE statement_id = '{id}'"),
        &[],
        1,
    );
    let (_, rows) = rows?;
    Ok(build_plan(&rows, text))
}

fn build_plan(rows: &[Row], text: String) -> Plan {
    let mut nodes: Vec<PlanNode> = rows
        .iter()
        .map(|r| {
            let op = [s(r, 2), s(r, 3)]
                .iter()
                .filter(|x| !x.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            PlanNode {
                id: s(r, 0).parse().unwrap_or(0),
                parent_id: num(r, 1).map(|v| v as u32),
                operation: op,
                object_owner: opt(r, 4),
                object_name: opt(r, 5),
                object_alias: opt(r, 6),
                cost: num(r, 7),
                rows: num(r, 8),
                bytes: num(r, 9),
                seconds: num(r, 10),
                access: opt(r, 11),
                filter: opt(r, 12),
                ..Default::default()
            }
        })
        .collect();
    let ids: Vec<u32> = nodes.iter().map(|n| n.id).collect();
    // Depth and child counts.
    for i in 0..nodes.len() {
        let mut d = 0;
        let mut p = nodes[i].parent_id;
        while let Some(pid) = p {
            d += 1;
            p = ids
                .iter()
                .position(|x| *x == pid)
                .and_then(|k| nodes[k].parent_id);
            if d > 200 {
                break;
            }
        }
        nodes[i].depth = d;
        if let Some(pid) = nodes[i].parent_id {
            if let Some(k) = ids.iter().position(|x| *x == pid) {
                nodes[k].children += 1;
            }
        }
    }
    // Self cost and shares.
    let total = nodes
        .iter()
        .find(|n| n.parent_id.is_none())
        .and_then(|n| n.cost);
    let child_sum: Vec<f64> = nodes
        .iter()
        .map(|n| {
            nodes
                .iter()
                .filter(|c| c.parent_id == Some(n.id))
                .filter_map(|c| c.cost)
                .sum()
        })
        .collect();
    for (i, n) in nodes.iter_mut().enumerate() {
        n.self_cost = (n.cost.unwrap_or(0.0) - child_sum[i]).max(0.0);
        n.cost_pct = match total {
            Some(t) if t > 0.0 => (n.self_cost / t * 100.0).clamp(0.0, 100.0),
            _ => 0.0,
        };
    }
    // The most expensive path: from the root, always into the child with the larger cost.
    let mut cur = nodes.iter().find(|n| n.parent_id.is_none()).map(|n| n.id);
    while let Some(id) = cur {
        if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
            n.hot = true;
        }
        cur = nodes
            .iter()
            .filter(|c| c.parent_id == Some(id))
            .max_by(|a, b| a.cost.unwrap_or(0.0).total_cmp(&b.cost.unwrap_or(0.0)))
            .map(|c| c.id);
    }
    let notes = parse_notes(&text);
    let findings = analyze_plan(&nodes, &notes);
    Plan {
        nodes,
        total_cost: total,
        notes,
        text,
        findings,
    }
}

/// Lines under the `Note` heading of a `DBMS_XPLAN` listing.
pub fn parse_notes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_notes = false;
    for line in text.lines() {
        let t = line.trim();
        if t == "Note" {
            in_notes = true;
        } else if in_notes {
            match t.strip_prefix("- ") {
                Some(n) => out.push(n.trim().to_string()),
                None if t.is_empty() || t.starts_with('-') && t.chars().all(|c| c == '-') => {}
                None => in_notes = false,
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------- rules

const BIG: f64 = 100_000.0;
const MEDIUM: f64 = 10_000.0;

/// `"ALIAS"."COLUMN"` pairs in a predicate, in order of appearance.
fn qualified_columns(pred: &str) -> Vec<(String, String, bool)> {
    let b: Vec<char> = pred.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '"' {
            let s1 = i + 1;
            let mut e1 = s1;
            while e1 < b.len() && b[e1] != '"' {
                e1 += 1;
            }
            if e1 + 2 < b.len() && b[e1 + 1] == '.' && b[e1 + 2] == '"' {
                let s2 = e1 + 3;
                let mut e2 = s2;
                while e2 < b.len() && b[e2] != '"' {
                    e2 += 1;
                }
                let after: String = b[(e2 + 1).min(b.len())..]
                    .iter()
                    .take(3)
                    .collect::<String>()
                    .trim_start()
                    .chars()
                    .take(2)
                    .collect();
                let equality = after.starts_with('=') && !after.starts_with("=>");
                out.push((
                    b[s1..e1].iter().collect(),
                    b[s2..e2.min(b.len())].iter().collect(),
                    equality,
                ));
                i = e2 + 1;
                continue;
            }
            i = e1 + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Columns of `alias` used in a filter, equality comparisons first, at most three.
pub fn index_candidates(pred: &str, alias: &str) -> Vec<String> {
    let mut eq: Vec<String> = Vec::new();
    let mut other: Vec<String> = Vec::new();
    for (a, c, is_eq) in qualified_columns(pred) {
        if !a.eq_ignore_ascii_case(alias) {
            continue;
        }
        if eq.contains(&c) || other.contains(&c) {
            continue;
        }
        if is_eq {
            eq.push(c);
        } else {
            other.push(c);
        }
    }
    eq.into_iter().chain(other).fold(Vec::new(), |mut v, c| {
        if !v.contains(&c) && v.len() < 3 {
            v.push(c);
        }
        v
    })
}

fn alias_of(n: &PlanNode) -> String {
    n.object_alias
        .as_deref()
        .and_then(|a| a.split('@').next())
        .map(|a| a.trim_matches('"').to_string())
        .or_else(|| n.object_name.clone())
        .unwrap_or_default()
}

fn fmt_rows(r: f64) -> String {
    if r >= 1e9 {
        format!("{:.1} billion", r / 1e9)
    } else if r >= 1e6 {
        format!("{:.1} million", r / 1e6)
    } else if r >= 1e3 {
        format!("{:.0},{:03.0}", (r / 1e3).floor(), r % 1e3)
    } else {
        format!("{r:.0}")
    }
}

fn finding(
    sev: &str,
    code: &str,
    title: String,
    detail: String,
    steps: &[u32],
    suggestion: Option<String>,
) -> Finding {
    Finding {
        severity: sev.into(),
        code: code.into(),
        title,
        detail,
        steps: steps.to_vec(),
        suggestion,
    }
}

/// The rules. Pure function of the plan, so it is easy to test.
pub fn analyze_plan(nodes: &[PlanNode], notes: &[String]) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for n in nodes {
        let op = n.operation.to_uppercase();
        let preds = [n.filter.as_deref(), n.access.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" AND ");
        let table = n.object_name.clone().unwrap_or_default();

        if op == "TABLE ACCESS FULL" && n.rows.unwrap_or(0.0) >= MEDIUM {
            let r = n.rows.unwrap_or(0.0);
            let cols = n
                .filter
                .as_deref()
                .map(|f| index_candidates(f, &alias_of(n)))
                .unwrap_or_default();
            let (detail, suggestion) = if n.filter.is_some() && !cols.is_empty() {
                (
                    format!("Step {} reads the whole table {} (about {} rows estimated) and then discards rows with a filter on {}.", n.id, table, fmt_rows(r), cols.join(", ")),
                    Some(format!("If this filter is selective and the statement runs often, an index on {}({}) may let Oracle read only the matching rows. Check the existing indexes first, and remember every index slows inserts and updates.", table, cols.join(", "))),
                )
            } else {
                (format!("Step {} reads the whole table {} (about {} rows estimated).", n.id, table, fmt_rows(r)), Some("This is fine when you really need most of the rows; otherwise add a selective condition on an indexed column.".into()))
            };
            out.push(finding(
                if r >= BIG { "high" } else { "warn" },
                "full_scan",
                format!("Full scan of {table}"),
                detail,
                &[n.id],
                suggestion,
            ));
        }
        if op.contains("CARTESIAN") {
            out.push(finding("high", "cartesian", "Cartesian product".into(), format!("Step {} pairs every row of one input with every row of the other. A join condition is probably missing or cannot be used.", n.id), &[n.id], Some("Check the join conditions between the tables in the FROM clause; each table should be linked to another one.".into())));
        }
        if let Some(col) = preds
            .find("INTERNAL_FUNCTION(")
            .map(|p| &preds[p + "INTERNAL_FUNCTION(".len()..])
        {
            let colname: String = col.chars().take_while(|c| *c != ')').collect();
            out.push(finding("high", "implicit_conversion", format!("Implicit data type conversion on {colname}"), format!("Step {} converts the column {} to compare it, so an index on that column cannot be used.", n.id, colname), &[n.id], Some("Compare the column with a value of the same data type (for example a number with a number, a date with a DATE literal) instead of relying on automatic conversion.".into())));
        } else {
            for f in ["UPPER(", "LOWER(", "TO_CHAR(", "TRUNC(", "SUBSTR(", "NVL("] {
                let pat = format!("{f}\"");
                if let Some(p) = preds.find(&pat) {
                    let col: String = preds[p + pat.len() - 1..]
                        .chars()
                        .take_while(|c| *c != ')')
                        .collect();
                    out.push(finding("warn", "function_on_column", format!("{}) applied to a column", f.trim_end_matches('(')), format!("Step {} applies {}…) to {}, so a normal index on that column cannot be used.", n.id, f.trim_end_matches('('), col), &[n.id], Some("Rewrite the condition so the column is compared as it is, or create a function-based index on the same expression.".into())));
                    break;
                }
            }
        }
        if op == "NESTED LOOPS" && n.children >= 2 {
            let kids: Vec<&PlanNode> = nodes.iter().filter(|c| c.parent_id == Some(n.id)).collect();
            if let (Some(outer), Some(inner)) = (kids.first(), kids.get(1)) {
                if outer.rows.unwrap_or(0.0) >= 50_000.0
                    && inner.operation.to_uppercase() == "TABLE ACCESS FULL"
                {
                    out.push(finding("warn", "nested_loops_full_scan", "Nested loops with a full scan inside".into(), format!("Step {} repeats the full scan in step {} for each of about {} outer rows.", n.id, inner.id, fmt_rows(outer.rows.unwrap_or(0.0))), &[n.id, inner.id], Some("Usually a hash join or an index on the join column is better here. Check that the table statistics are current.".into())));
                }
            }
        }
        if (op == "INDEX FULL SCAN" || op == "INDEX SKIP SCAN") && n.rows.unwrap_or(0.0) >= MEDIUM {
            out.push(finding("info", "index_scan_wide", format!("{} on {}", op.to_lowercase(), n.object_name.clone().unwrap_or_default()), format!("Step {} reads a large part of the index (about {} rows estimated); the index does not match the filter columns well.", n.id, fmt_rows(n.rows.unwrap_or(0.0))), &[n.id], Some("An index that starts with the columns used in the filter would be read more selectively.".into())));
        }
        if (op.starts_with("SORT") || op.starts_with("HASH GROUP BY"))
            && n.bytes.unwrap_or(0.0) >= 1e9
        {
            out.push(finding("warn", "large_sort", format!("Large {}", op.to_lowercase()), format!("Step {} handles about {:.1} GB; it may spill to temporary space and be slow.", n.id, n.bytes.unwrap_or(0.0) / 1e9), &[n.id], Some("Reduce the rows before this step (filter earlier, select fewer columns) if you can.".into())));
        }
        if op == "FILTER" && n.children >= 2 {
            let first = nodes.iter().find(|c| c.parent_id == Some(n.id));
            if first.and_then(|f| f.rows).unwrap_or(0.0) >= 1000.0 {
                out.push(finding("info", "filter_subquery", "Subquery checked row by row".into(), format!("Step {} evaluates a subquery for each row coming from step {}.", n.id, first.map(|f| f.id).unwrap_or(0)), &[n.id], Some("Rewriting the subquery as a join, or as EXISTS with a supporting index, often lets Oracle do this in one pass.".into())));
            }
        }
    }
    for note in notes {
        let l = note.to_lowercase();
        if l.contains("dynamic statistics") || l.contains("dynamic sampling") {
            out.push(finding("warn", "missing_statistics", "The optimizer had to sample the data".into(), format!("Note from Oracle: {note}. Table statistics are missing or stale, so the estimates below may be far off."), &[], Some("Ask your DBA to gather statistics on the tables involved (DBMS_STATS.GATHER_TABLE_STATS).".into())));
        } else if l.contains("baseline") || l.contains("sql profile") || l.contains("outline") {
            out.push(finding("info", "plan_pinned", "A stored plan is being used".into(), format!("Note from Oracle: {note}. Changing the statement or the statistics may not change the plan."), &[], None));
        } else if l.contains("adaptive plan") {
            out.push(finding("info", "adaptive", "Adaptive plan".into(), "Oracle may switch parts of this plan while the statement runs, so the real plan can differ from this estimate.".into(), &[], None));
        }
    }
    if let Some(hot) = nodes
        .iter()
        .filter(|n| n.cost_pct >= 50.0)
        .max_by(|a, b| a.cost_pct.total_cmp(&b.cost_pct))
    {
        out.push(finding(
            "info",
            "hot_step",
            format!("{:.0}% of the estimated cost is one step", hot.cost_pct),
            format!(
                "Step {} ({}{}) accounts for most of the estimated cost. Start there.",
                hot.id,
                hot.operation,
                hot.object_name
                    .as_deref()
                    .map(|o| format!(" on {o}"))
                    .unwrap_or_default()
            ),
            &[hot.id],
            None,
        ));
    }
    let rank = |f: &Finding| match f.severity.as_str() {
        "high" => 0,
        "warn" => 1,
        _ => 2,
    };
    out.sort_by_key(rank);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn node(id: u32, parent: Option<u32>, op: &str) -> PlanNode {
        PlanNode {
            id,
            parent_id: parent,
            operation: op.into(),
            ..Default::default()
        }
    }
    fn full_scan(id: u32, parent: u32, table: &str, rows: f64, filter: Option<&str>) -> PlanNode {
        PlanNode {
            object_name: Some(table.into()),
            object_alias: Some(format!("\"{}\"@\"SEL$1\"", table.chars().next().unwrap())),
            rows: Some(rows),
            filter: filter.map(String::from),
            cost: Some(500.0),
            ..node(id, Some(parent), "TABLE ACCESS FULL")
        }
    }
    fn codes(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|x| x.code.as_str()).collect()
    }

    #[test]
    fn a_big_filtered_full_scan_suggests_an_index_on_the_filter_columns() {
        let n = full_scan(
            2,
            1,
            "ORDERS",
            2_500_000.0,
            Some("\"O\".\"STATUS\"='OPEN' AND \"O\".\"CUSTOMER_ID\"=:1 AND \"O\".\"TOTAL\">100"),
        );
        let f = analyze_plan(&[node(1, None, "SELECT STATEMENT"), n], &[]);
        assert_eq!(codes(&f), vec!["full_scan"]);
        assert_eq!(f[0].severity, "high");
        assert!(
            f[0].title == "Full scan of ORDERS" && f[0].detail.contains("2,500,000")
                || f[0].detail.contains("2.5 million"),
            "{}",
            f[0].detail
        );
        let s = f[0].suggestion.as_deref().unwrap();
        assert!(
            s.contains("ORDERS(STATUS, CUSTOMER_ID, TOTAL)")
                || s.contains("ORDERS(CUSTOMER_ID, STATUS, TOTAL)"),
            "{s}"
        );
        assert!(
            s.contains("Check the existing indexes"),
            "never suggests blindly: {s}"
        );
        assert_eq!(
            index_candidates("\"O\".\"A\"=1 AND \"O\".\"B\">2 AND \"X\".\"C\"=3", "O"),
            vec!["A", "B"],
            "equality columns first, other tables ignored"
        );
    }

    #[test]
    fn small_scans_are_not_worth_a_finding() {
        assert!(analyze_plan(
            &[
                node(1, None, "SELECT STATEMENT"),
                full_scan(2, 1, "COUNTRIES", 250.0, None)
            ],
            &[]
        )
        .is_empty());
        let f = analyze_plan(
            &[
                node(1, None, "SELECT STATEMENT"),
                full_scan(2, 1, "AUDIT", 40_000.0, None),
            ],
            &[],
        );
        assert_eq!(
            (f[0].severity.as_str(), f[0].code.as_str()),
            ("warn", "full_scan")
        );
    }

    #[test]
    fn missing_join_conditions_and_implicit_conversions_are_high_severity() {
        let f = analyze_plan(
            &[
                node(1, None, "SELECT STATEMENT"),
                node(2, Some(1), "MERGE JOIN CARTESIAN"),
            ],
            &[],
        );
        assert_eq!(
            (codes(&f), f[0].severity.as_str()),
            (vec!["cartesian"], "high")
        );
        let mut idx = node(3, Some(1), "INDEX RANGE SCAN");
        idx.access = Some("INTERNAL_FUNCTION(\"E\".\"PHONE\")=5551234".into());
        let f = analyze_plan(&[node(1, None, "SELECT STATEMENT"), idx], &[]);
        assert_eq!(codes(&f), vec!["implicit_conversion"]);
        assert!(f[0].title.contains("\"E\".\"PHONE\""), "{}", f[0].title);
    }

    #[test]
    fn functions_on_columns_and_nested_loops_over_big_outer_inputs() {
        let mut t = full_scan(
            2,
            1,
            "EMP",
            100.0,
            Some("UPPER(\"E\".\"LAST_NAME\")='KING'"),
        );
        t.rows = Some(100.0);
        let f = analyze_plan(&[node(1, None, "SELECT STATEMENT"), t], &[]);
        assert_eq!(codes(&f), vec!["function_on_column"]);
        let nl = PlanNode {
            children: 2,
            ..node(1, None, "NESTED LOOPS")
        };
        let outer = PlanNode {
            rows: Some(200_000.0),
            ..node(2, Some(1), "INDEX FULL SCAN")
        };
        let inner = PlanNode {
            object_name: Some("B".into()),
            rows: Some(5.0),
            ..node(3, Some(1), "TABLE ACCESS FULL")
        };
        let f = analyze_plan(&[nl, outer, inner], &[]);
        assert!(
            codes(&f).contains(&"nested_loops_full_scan"),
            "{:?}",
            codes(&f)
        );
    }

    #[test]
    fn notes_from_oracle_become_findings() {
        let text = "Plan hash value: 1\n\n---\n\nNote\n-----\n   - dynamic statistics used: dynamic sampling (level=2)\n   - SQL plan baseline \"SQL_PLAN_x\" used for this statement\n";
        assert_eq!(parse_notes(text).len(), 2);
        let f = analyze_plan(&[], &parse_notes(text));
        assert_eq!(codes(&f), vec!["missing_statistics", "plan_pinned"]);
        assert!(parse_notes("no notes here").is_empty());
    }

    fn row(v: &[&str]) -> Vec<Option<String>> {
        v.iter()
            .map(|c| {
                if c.is_empty() {
                    None
                } else {
                    Some(c.to_string())
                }
            })
            .collect()
    }

    #[test]
    fn the_tree_has_depths_self_costs_shares_and_a_hot_path() {
        // id, parent, op, options, owner, name, alias, cost, rows, bytes, time, access, filter
        let rows = vec![
            row(&[
                "0",
                "",
                "SELECT STATEMENT",
                "",
                "",
                "",
                "",
                "1000",
                "50",
                "5000",
                "1",
                "",
                "",
            ]),
            row(&[
                "1",
                "0",
                "HASH JOIN",
                "",
                "",
                "",
                "",
                "1000",
                "50",
                "5000",
                "1",
                "\"E\".\"DEPT\"=\"D\".\"ID\"",
                "",
            ]),
            row(&[
                "2",
                "1",
                "TABLE ACCESS",
                "FULL",
                "HR",
                "DEPARTMENTS",
                "\"D\"@\"SEL$1\"",
                "100",
                "27",
                "600",
                "1",
                "",
                "",
            ]),
            row(&[
                "3",
                "1",
                "TABLE ACCESS",
                "FULL",
                "HR",
                "EMPLOYEES",
                "\"E\"@\"SEL$1\"",
                "880",
                "20000",
                "400000",
                "1",
                "",
                "\"E\".\"SALARY\">5000",
            ]),
        ];
        let p = build_plan(&rows, String::new());
        assert_eq!(p.total_cost, Some(1000.0));
        let by = |id: u32| p.nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!((by(0).depth, by(1).depth, by(3).depth), (0, 1, 2));
        assert_eq!(by(1).children, 2);
        assert_eq!(by(1).self_cost, 20.0, "1000 - (100 + 880)");
        assert_eq!(by(3).cost_pct, 88.0);
        assert!(
            by(0).hot && by(1).hot && by(3).hot && !by(2).hot,
            "the hot path follows the costlier child"
        );
        assert!(p
            .findings
            .iter()
            .any(|f| f.code == "hot_step" && f.steps == vec![3]));
        assert!(
            p.findings.iter().any(|f| f.code == "full_scan"),
            "20,000 rows is a warning"
        );
        assert_eq!(by(2).operation, "TABLE ACCESS FULL");
    }

    #[test]
    fn explain_runs_the_plan_reads_it_and_cleans_up() {
        let db = FakeDb::new()
            .with_affected(0)
            .with_table(
                "pl:rows_full",
                FakeTable::new(
                    &[
                        "i", "p", "o", "op", "ow", "on", "al", "c", "r", "b", "t", "a", "f",
                    ],
                    vec![
                        vec![
                            Some("0"),
                            None,
                            Some("SELECT STATEMENT"),
                            None,
                            None,
                            None,
                            None,
                            Some("5"),
                            Some("1"),
                            Some("10"),
                            Some("1"),
                            None,
                            None,
                        ],
                        vec![
                            Some("1"),
                            Some("0"),
                            Some("INDEX"),
                            Some("UNIQUE SCAN"),
                            Some("HR"),
                            Some("EMP_PK"),
                            None,
                            Some("5"),
                            Some("1"),
                            Some("10"),
                            Some("1"),
                            Some("\"EMPLOYEE_ID\"=100"),
                            None,
                        ],
                    ],
                ),
            )
            .with_table(
                "pl:text",
                FakeTable::new(
                    &["t"],
                    vec![
                        vec![Some("Plan hash value: 1")],
                        vec![Some("Note")],
                        vec![Some("-----")],
                        vec![Some(
                            "   - dynamic statistics used: dynamic sampling (level=2)",
                        )],
                    ],
                ),
            );
        let p = explain(&db, "SELECT * FROM employees WHERE employee_id = 100;").unwrap();
        assert_eq!(p.nodes.len(), 2);
        assert_eq!(p.nodes[1].operation, "INDEX UNIQUE SCAN");
        assert!(p.findings.iter().any(|f| f.code == "missing_statistics"));
        let calls = db.log.lock().unwrap().calls.clone();
        let first = &calls[0].0;
        assert!(
            first.starts_with("EXPLAIN PLAN SET STATEMENT_ID = 'ORAIDE")
                && first.ends_with("FOR SELECT * FROM employees WHERE employee_id = 100"),
            "{first}"
        );
        assert!(
            calls
                .last()
                .unwrap()
                .0
                .starts_with("DELETE FROM plan_table"),
            "the plan rows are removed again"
        );
    }

    #[test]
    fn an_old_private_plan_table_without_newer_columns_still_works() {
        let db = FakeDb::new()
            .with_affected(0)
            .with_error(
                "pl:rows_full",
                "ORA-00904: \"OBJECT_ALIAS\": invalid identifier",
            )
            .with_table(
                "pl:rows_old",
                FakeTable::new(
                    &[
                        "i", "p", "o", "op", "ow", "on", "al", "c", "r", "b", "t", "a", "f",
                    ],
                    vec![vec![
                        Some("0"),
                        None,
                        Some("SELECT STATEMENT"),
                        None,
                        None,
                        None,
                        None,
                        Some("2"),
                        Some("1"),
                        Some("5"),
                        Some("1"),
                        None,
                        None,
                    ]],
                ),
            );
        let p = explain(&db, "SELECT 1 FROM dual").unwrap();
        assert_eq!(p.nodes.len(), 1);
    }

    #[test]
    fn a_statement_oracle_cannot_plan_reports_oracles_error() {
        let db =
            FakeDb::new().with_error("explain plan", "ORA-00942: table or view does not exist");
        assert!(explain(&db, "SELECT * FROM nope")
            .unwrap_err()
            .to_string()
            .contains("ORA-00942"));
    }
}
