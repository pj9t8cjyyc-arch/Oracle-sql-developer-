//! Package explorer: members with signatures, what each package calls, which tables it reads or
//! writes, and which public members nothing references.
//!
//! Signatures come from `ALL_PROCEDURES` / `ALL_ARGUMENTS` (exact). Calls and table usage come
//! from a lexical scan of the package bodies (**heuristic**: it ignores dynamic SQL, takes
//! `FROM a, b` lists only up to the first table, and cannot see callers outside the schema, jobs
//! or applications). Wrapped bodies cannot be scanned and are reported as such.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::db::{collect, Database, Row};
use crate::lexer::{lex, Tok, Token};
use crate::Result;

#[derive(Debug, Clone, Default)]
pub struct PkgMeta {
    pub name: String,
    pub spec_status: String,
    pub body_status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pkg: String,
    pub name: String,
    pub overload: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ArgRow {
    pub pkg: String,
    pub name: String,
    pub overload: Option<String>,
    pub position: i64,
    pub arg_name: Option<String>,
    pub in_out: String,
    pub data_type: String,
}

#[derive(Debug, Default)]
pub struct Inputs {
    pub owner: String,
    pub packages: Vec<PkgMeta>,
    pub procs: Vec<ProcRow>,
    pub args: Vec<ArgRow>,
    pub tables: Vec<String>,
    /// Package body source by package name.
    pub bodies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemberRef {
    pub package: String,
    pub member: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Member {
    pub name: String,
    /// `PROCEDURE` or `FUNCTION`.
    pub kind: String,
    pub signature: String,
    pub overload: Option<String>,
    /// Declared in the package specification.
    pub public: bool,
    pub calls: Vec<MemberRef>,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    /// `PACKAGE.MEMBER` callers in other packages.
    pub called_by: Vec<MemberRef>,
    /// Calls from inside the same package.
    pub internal_refs: usize,
    /// `used`, `unreferenced` (public, nothing found calls it) or `unused_private`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TableUse {
    pub table: String,
    pub read: bool,
    pub write: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageDetail {
    pub name: String,
    pub spec_status: String,
    pub body_status: Option<String>,
    pub wrapped: bool,
    pub body_lines: usize,
    pub members: Vec<Member>,
    pub calls_packages: Vec<String>,
    pub called_by: Vec<String>,
    pub tables: Vec<TableUse>,
    pub unreferenced: usize,
    pub unused_private: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageAnalysis {
    pub owner: String,
    pub packages: Vec<PackageDetail>,
    pub wrapped: usize,
    pub invalid: usize,
}

fn s(r: &Row, i: usize) -> String {
    r.get(i).cloned().flatten().unwrap_or_default()
}
fn opt(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten().filter(|v| !v.is_empty())
}

/// Loads everything for one schema (five dictionary queries; the body query is the large one).
pub fn load_inputs(db: &dyn Database, owner: &str) -> Result<Inputs> {
    let b = [owner.to_string()];
    let mut metas: BTreeMap<String, PkgMeta> = BTreeMap::new();
    let (_, rows) = collect(
        db,
        "SELECT /*pkg:objs*/ object_name, object_type, status FROM all_objects WHERE owner = :1 AND object_type IN ('PACKAGE','PACKAGE BODY')",
        &b,
    )?;
    for r in &rows {
        let m = metas.entry(s(r, 0)).or_insert_with(|| PkgMeta {
            name: s(r, 0),
            ..Default::default()
        });
        if s(r, 1) == "PACKAGE" {
            m.spec_status = s(r, 2);
        } else {
            m.body_status = Some(s(r, 2));
        }
    }
    let (_, rows) = collect(
        db,
        "SELECT /*pkg:procs*/ object_name, procedure_name, overload FROM all_procedures WHERE owner = :1 AND object_type = 'PACKAGE' AND procedure_name IS NOT NULL ORDER BY object_name, subprogram_id",
        &b,
    )?;
    let procs = rows
        .iter()
        .map(|r| ProcRow {
            pkg: s(r, 0),
            name: s(r, 1),
            overload: opt(r, 2),
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT /*pkg:args*/ package_name, object_name, overload, position, argument_name, in_out, NVL(pls_type, data_type) FROM all_arguments WHERE owner = :1 AND package_name IS NOT NULL AND data_level = 0 ORDER BY package_name, object_name, overload, sequence",
        &b,
    )?;
    let args = rows
        .iter()
        .map(|r| ArgRow {
            pkg: s(r, 0),
            name: s(r, 1),
            overload: opt(r, 2),
            position: s(r, 3).parse().unwrap_or(0),
            arg_name: opt(r, 4),
            in_out: s(r, 5),
            data_type: s(r, 6),
        })
        .collect();
    let (_, rows) = collect(
        db,
        "SELECT /*pkg:tables*/ object_name FROM all_objects WHERE owner = :1 AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW')",
        &b,
    )?;
    let tables = rows.iter().map(|r| s(r, 0)).collect();
    let (_, rows) = collect(
        db,
        "SELECT /*pkg:bodies*/ name, text FROM all_source WHERE owner = :1 AND type = 'PACKAGE BODY' ORDER BY name, line",
        &b,
    )?;
    let mut bodies: BTreeMap<String, String> = BTreeMap::new();
    for r in &rows {
        bodies.entry(s(r, 0)).or_default().push_str(&s(r, 1));
    }
    Ok(Inputs {
        owner: owner.to_string(),
        packages: metas.into_values().collect(),
        procs,
        args,
        tables,
        bodies,
    })
}

pub fn analyze_schema(db: &dyn Database, owner: &str) -> Result<PackageAnalysis> {
    Ok(analyze(&load_inputs(db, owner)?))
}

fn is_wrapped(body: &str) -> bool {
    body.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.to_lowercase().split_whitespace().last() == Some("wrapped"))
        .unwrap_or(false)
}

fn signature(name: &str, args: &[&ArgRow]) -> (String, String) {
    let ret = args
        .iter()
        .find(|a| a.position == 0 && a.arg_name.is_none());
    let params: Vec<String> = args
        .iter()
        .filter(|a| a.position > 0 && a.arg_name.is_some())
        .map(|a| {
            format!(
                "{} {} {}",
                a.arg_name.as_deref().unwrap_or(""),
                a.in_out.replace("IN/OUT", "IN OUT"),
                a.data_type
            )
        })
        .collect();
    let mut sig = name.to_string();
    if !params.is_empty() {
        sig.push_str(&format!("({})", params.join(", ")));
    }
    let kind = if let Some(r) = ret {
        sig.push_str(&format!(" RETURN {}", r.data_type));
        "FUNCTION"
    } else {
        "PROCEDURE"
    };
    (kind.to_string(), sig)
}

struct Found {
    name: String,
    kind: String,
    start: usize,
    sig: String,
}

fn word(t: &Token) -> Option<&str> {
    match &t.tok {
        Tok::Word(w) => Some(w),
        Tok::Quoted(q) => Some(q),
        _ => None,
    }
}

/// `PROCEDURE|FUNCTION name` at any depth, except `END name`-style tails.
fn member_starts(toks: &[Token], src: &[char]) -> Vec<Found> {
    let mut out = Vec::new();
    for i in 0..toks.len().saturating_sub(1) {
        let kind = match &toks[i].tok {
            Tok::Word(w) if w == "PROCEDURE" || w == "FUNCTION" => w.clone(),
            _ => continue,
        };
        if i > 0 && matches!(&toks[i - 1].tok, Tok::Word(w) if w == "END") {
            continue;
        }
        let Some(name) = word(&toks[i + 1]) else {
            continue;
        };
        // Signature text: up to IS / AS / `;` (declaration), whitespace collapsed.
        let mut end = i + 1;
        while end < toks.len()
            && !matches!(&toks[end].tok, Tok::Word(w) if w == "IS" || w == "AS")
            && !matches!(toks[end].tok, Tok::Punct(';'))
        {
            end += 1;
        }
        let a = toks[i + 1].start;
        let z = toks.get(end).map(|t| t.start).unwrap_or(src.len());
        let sig = src[a..z.min(src.len())]
            .iter()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Found {
            name: name.to_uppercase(),
            kind,
            start: i,
            sig,
        });
    }
    out
}

#[derive(Default)]
struct Scan {
    calls: BTreeSet<(String, String)>,
    reads: BTreeSet<String>,
    writes: BTreeSet<String>,
    internal: HashMap<String, usize>,
}

fn prev_word(toks: &[Token], j: usize, n: usize) -> Option<&str> {
    j.checked_sub(n)
        .and_then(|k| toks.get(k))
        .and_then(|t| match &t.tok {
            Tok::Word(w) => Some(w.as_str()),
            _ => None,
        })
}

#[allow(clippy::too_many_arguments)]
fn scan_span(
    toks: &[Token],
    from: usize,
    to: usize,
    owner: &str,
    pkg: &str,
    pkgs: &HashSet<String>,
    tables: &HashSet<String>,
    own_members: &HashSet<String>,
    skip: usize,
) -> Scan {
    let mut sc = Scan::default();
    let tk = |k: usize| toks.get(k).map(|t| &t.tok);
    let mut j = from;
    while j < to {
        let Some(w) = word(&toks[j]).map(|w| w.to_uppercase()) else {
            j += 1;
            continue;
        };
        let after_dot = j > 0 && matches!(toks[j - 1].tok, Tok::Punct('.'));
        let mut consumed = 1;
        if !after_dot && j != skip {
            // OWNER.PKG.MEMBER
            if w == owner
                && matches!(tk(j + 1), Some(Tok::Punct('.')))
                && matches!(tk(j + 3), Some(Tok::Punct('.')))
            {
                if let (Some(p), Some(m)) = (
                    toks.get(j + 2).and_then(word),
                    toks.get(j + 4).and_then(word),
                ) {
                    if pkgs.contains(&p.to_uppercase()) {
                        sc.calls.insert((p.to_uppercase(), m.to_uppercase()));
                        consumed = 5;
                    }
                }
            }
            // OWNER.TABLE
            if consumed == 1 && w == owner && matches!(tk(j + 1), Some(Tok::Punct('.'))) {
                if let Some(t) = toks.get(j + 2).and_then(word) {
                    if tables.contains(&t.to_uppercase()) {
                        classify(&mut sc, &t.to_uppercase(), toks, j);
                        consumed = 3;
                    }
                }
            }
            // PKG.MEMBER, TABLE.column
            if consumed == 1 && matches!(tk(j + 1), Some(Tok::Punct('.'))) {
                if pkgs.contains(&w) {
                    if let Some(m) = toks.get(j + 2).and_then(word) {
                        let m = m.to_uppercase();
                        if w == pkg {
                            *sc.internal.entry(m).or_default() += 1;
                        } else {
                            sc.calls.insert((w.clone(), m));
                        }
                        consumed = 3;
                    }
                } else if tables.contains(&w) {
                    classify(&mut sc, &w, toks, j);
                    consumed = 3;
                }
            }
            // Bare TABLE after a table-introducing keyword.
            if consumed == 1
                && tables.contains(&w)
                && matches!(
                    prev_word(toks, j, 1),
                    Some("FROM" | "JOIN" | "INTO" | "UPDATE" | "USING" | "TABLE")
                )
            {
                classify(&mut sc, &w, toks, j);
            }
            // Bare call to a member of this package.
            if consumed == 1
                && own_members.contains(&w)
                && matches!(tk(j + 1), Some(Tok::Punct('(' | ';')))
                && !matches!(
                    prev_word(toks, j, 1),
                    Some("PROCEDURE" | "FUNCTION" | "END")
                )
            {
                *sc.internal.entry(w.clone()).or_default() += 1;
            }
        }
        j += consumed;
    }
    sc
}

fn classify(sc: &mut Scan, table: &str, toks: &[Token], j: usize) {
    // `j` is the first token of the table reference (a schema prefix counts as part of it).
    let write = match (prev_word(toks, j, 1), prev_word(toks, j, 2)) {
        (Some("INTO"), Some("INSERT" | "MERGE")) => true,
        (Some("UPDATE"), _) => true,
        (Some("FROM"), Some("DELETE")) => true,
        (Some("INTO"), None) => false,
        _ => false,
    };
    if write {
        sc.writes.insert(table.to_string());
    } else {
        sc.reads.insert(table.to_string());
    }
}

pub fn analyze(inp: &Inputs) -> PackageAnalysis {
    let owner = inp.owner.to_uppercase();
    let pkgs: HashSet<String> = inp.packages.iter().map(|p| p.name.to_uppercase()).collect();
    let tables: HashSet<String> = inp.tables.iter().map(|t| t.to_uppercase()).collect();
    let mut details: BTreeMap<String, PackageDetail> = BTreeMap::new();
    // (callee package, member) -> callers
    let mut callers: HashMap<(String, String), Vec<MemberRef>> = HashMap::new();
    let mut raw: BTreeMap<String, Vec<(Member, Scan)>> = BTreeMap::new();

    for meta in &inp.packages {
        let pname = meta.name.to_uppercase();
        let body = inp.bodies.get(&meta.name).map(String::as_str).unwrap_or("");
        let wrapped = is_wrapped(body);
        // Public members and their exact signatures.
        let mut members: Vec<Member> = Vec::new();
        let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
        for p in inp.procs.iter().filter(|p| p.pkg == meta.name) {
            if !seen.insert((p.name.clone(), p.overload.clone())) {
                continue;
            }
            let args: Vec<&ArgRow> = inp
                .args
                .iter()
                .filter(|a| a.pkg == p.pkg && a.name == p.name && a.overload == p.overload)
                .collect();
            let (kind, sig) = signature(&p.name, &args);
            members.push(Member {
                name: p.name.to_uppercase(),
                kind,
                signature: sig,
                overload: p.overload.clone(),
                public: true,
                calls: vec![],
                reads: vec![],
                writes: vec![],
                called_by: vec![],
                internal_refs: 0,
                status: String::new(),
            });
        }
        let mut scans: Vec<Scan> = members.iter().map(|_| Scan::default()).collect();
        if !wrapped && !body.is_empty() {
            let chars: Vec<char> = body.chars().collect();
            let toks = lex(body);
            let found = member_starts(&toks, &chars);
            // Private members: defined in the body, not declared in the spec.
            let public: HashSet<String> = members.iter().map(|m| m.name.clone()).collect();
            for f in &found {
                if !public.contains(&f.name) && !members.iter().any(|m| m.name == f.name) {
                    members.push(Member {
                        name: f.name.clone(),
                        kind: f.kind.clone(),
                        signature: f.sig.clone(),
                        overload: None,
                        public: false,
                        calls: vec![],
                        reads: vec![],
                        writes: vec![],
                        called_by: vec![],
                        internal_refs: 0,
                        status: String::new(),
                    });
                    scans.push(Scan::default());
                }
            }
            let own: HashSet<String> = members.iter().map(|m| m.name.clone()).collect();
            for (k, f) in found.iter().enumerate() {
                let end = found.get(k + 1).map(|n| n.start).unwrap_or(toks.len());
                let sc = scan_span(
                    &toks,
                    f.start,
                    end,
                    &owner,
                    &pname,
                    &pkgs,
                    &tables,
                    &own,
                    f.start + 1,
                );
                if let Some(idx) = members.iter().position(|m| m.name == f.name) {
                    let t = &mut scans[idx];
                    t.calls.extend(sc.calls);
                    t.reads.extend(sc.reads);
                    t.writes.extend(sc.writes);
                    for (n, c) in sc.internal {
                        if n != f.name {
                            *t.internal.entry(n).or_default() += c;
                        } else {
                            // recursion is not a use
                        }
                    }
                }
            }
        }
        details.insert(
            pname.clone(),
            PackageDetail {
                name: pname.clone(),
                spec_status: meta.spec_status.clone(),
                body_status: meta.body_status.clone(),
                wrapped,
                body_lines: body.lines().count(),
                members: vec![],
                calls_packages: vec![],
                called_by: vec![],
                tables: vec![],
                unreferenced: 0,
                unused_private: 0,
            },
        );
        raw.insert(pname, members.into_iter().zip(scans).collect());
    }

    // Cross-package callers.
    for (pname, ms) in &raw {
        for (m, sc) in ms {
            for c in &sc.calls {
                callers.entry(c.clone()).or_default().push(MemberRef {
                    package: pname.clone(),
                    member: m.name.clone(),
                });
            }
        }
    }

    for (pname, ms) in raw {
        let mut internal_total: HashMap<String, usize> = HashMap::new();
        for (_, sc) in &ms {
            for (n, c) in &sc.internal {
                *internal_total.entry(n.clone()).or_default() += c;
            }
        }
        let d = details.get_mut(&pname).unwrap();
        let mut calls_pk: BTreeSet<String> = BTreeSet::new();
        let mut tabs: BTreeMap<String, (bool, bool)> = BTreeMap::new();
        for (mut m, sc) in ms {
            m.calls = sc
                .calls
                .iter()
                .map(|(p, n)| MemberRef {
                    package: p.clone(),
                    member: n.clone(),
                })
                .collect();
            for (p, _) in &sc.calls {
                calls_pk.insert(p.clone());
            }
            m.reads = sc.reads.iter().cloned().collect();
            m.writes = sc.writes.iter().cloned().collect();
            for t in &sc.reads {
                tabs.entry(t.clone()).or_default().0 = true;
            }
            for t in &sc.writes {
                tabs.entry(t.clone()).or_default().1 = true;
            }
            m.called_by = callers
                .get(&(pname.clone(), m.name.clone()))
                .cloned()
                .unwrap_or_default();
            m.internal_refs = internal_total.get(&m.name).copied().unwrap_or(0);
            let scanned = !d.wrapped && d.body_lines > 0;
            m.status = if m.called_by.is_empty() && m.internal_refs == 0 && scanned {
                if m.public {
                    d.unreferenced += 1;
                    "unreferenced"
                } else {
                    d.unused_private += 1;
                    "unused_private"
                }
            } else {
                "used"
            }
            .into();
            d.members.push(m);
        }
        d.calls_packages = calls_pk.into_iter().filter(|p| *p != pname).collect();
        d.tables = tabs
            .into_iter()
            .map(|(table, (read, write))| TableUse { table, read, write })
            .collect();
    }
    // Package-level "called by".
    let edges: Vec<(String, String)> = details
        .values()
        .flat_map(|d| {
            d.calls_packages
                .iter()
                .map(|c| (d.name.clone(), c.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    for (from, to) in edges {
        if let Some(d) = details.get_mut(&to) {
            d.called_by.push(from);
        }
    }
    let packages: Vec<PackageDetail> = details.into_values().collect();
    PackageAnalysis {
        owner: inp.owner.clone(),
        wrapped: packages.iter().filter(|p| p.wrapped).count(),
        invalid: packages
            .iter()
            .filter(|p| {
                p.spec_status != "VALID" || p.body_status.as_deref().is_some_and(|b| b != "VALID")
            })
            .count(),
        packages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeDb, FakeTable};

    fn inputs() -> Inputs {
        let meta = |n: &str, st: &str| PkgMeta {
            name: n.into(),
            spec_status: "VALID".into(),
            body_status: Some(st.into()),
        };
        let proc = |p: &str, n: &str, o: Option<&str>| ProcRow {
            pkg: p.into(),
            name: n.into(),
            overload: o.map(String::from),
        };
        let arg = |p: &str, n: &str, pos: i64, a: Option<&str>, io: &str, t: &str| ArgRow {
            pkg: p.into(),
            name: n.into(),
            overload: None,
            position: pos,
            arg_name: a.map(String::from),
            in_out: io.into(),
            data_type: t.into(),
        };
        let mut bodies = BTreeMap::new();
        bodies.insert(
            "EMP_PKG".to_string(),
            "PACKAGE BODY emp_pkg IS
  PROCEDURE log_it(p_msg VARCHAR2) IS
  BEGIN
    INSERT INTO audit_log(msg) VALUES (p_msg);
  END log_it;
  PROCEDURE helper IS BEGIN NULL; END helper;
  PROCEDURE raise_salary(p_id NUMBER, p_pct NUMBER) IS
  BEGIN
    UPDATE employees SET salary = salary * (1 + p_pct/100) WHERE employee_id = p_id;
    log_it('raised');
    util_pkg.notify('x');
  END raise_salary;
  FUNCTION headcount(p_dept NUMBER) RETURN NUMBER IS n NUMBER;
  BEGIN
    SELECT COUNT(*) INTO n FROM employees e JOIN departments d ON d.department_id = e.department_id;
    -- DELETE FROM never_seen
    RETURN n;
  END headcount;
END emp_pkg;"
                .into(),
        );
        bodies.insert(
            "UTIL_PKG".to_string(),
            "PACKAGE BODY util_pkg IS
  PROCEDURE notify(p VARCHAR2) IS BEGIN HR.emp_pkg.raise_salary(1, 2); DELETE FROM audit_log; END;
  PROCEDURE dead_one IS BEGIN NULL; END;
END util_pkg;"
                .into(),
        );
        bodies.insert(
            "SECRET_PKG".to_string(),
            "package body secret_pkg wrapped\na000000\n".into(),
        );
        Inputs {
            owner: "HR".into(),
            packages: vec![
                meta("EMP_PKG", "VALID"),
                meta("UTIL_PKG", "INVALID"),
                meta("SECRET_PKG", "VALID"),
            ],
            procs: vec![
                proc("EMP_PKG", "RAISE_SALARY", None),
                proc("EMP_PKG", "HEADCOUNT", None),
                proc("UTIL_PKG", "NOTIFY", None),
                proc("UTIL_PKG", "DEAD_ONE", None),
                proc("SECRET_PKG", "HIDDEN", None),
            ],
            args: vec![
                arg("EMP_PKG", "RAISE_SALARY", 1, Some("P_ID"), "IN", "NUMBER"),
                arg("EMP_PKG", "RAISE_SALARY", 2, Some("P_PCT"), "IN", "NUMBER"),
                arg("EMP_PKG", "HEADCOUNT", 0, None, "OUT", "NUMBER"),
                arg("EMP_PKG", "HEADCOUNT", 1, Some("P_DEPT"), "IN", "NUMBER"),
                arg("UTIL_PKG", "NOTIFY", 1, Some("P"), "IN", "VARCHAR2"),
            ],
            tables: vec!["EMPLOYEES".into(), "DEPARTMENTS".into(), "AUDIT_LOG".into()],
            bodies,
        }
    }

    fn pkg<'a>(a: &'a PackageAnalysis, n: &str) -> &'a PackageDetail {
        a.packages.iter().find(|p| p.name == n).unwrap()
    }
    fn mem<'a>(p: &'a PackageDetail, n: &str) -> &'a Member {
        p.members.iter().find(|m| m.name == n).unwrap()
    }

    #[test]
    fn exact_signatures_come_from_the_dictionary() {
        let a = analyze(&inputs());
        let e = pkg(&a, "EMP_PKG");
        assert_eq!(
            mem(e, "RAISE_SALARY").signature,
            "RAISE_SALARY(P_ID IN NUMBER, P_PCT IN NUMBER)"
        );
        assert_eq!(
            mem(e, "HEADCOUNT").signature,
            "HEADCOUNT(P_DEPT IN NUMBER) RETURN NUMBER"
        );
        assert_eq!(mem(e, "HEADCOUNT").kind, "FUNCTION");
        assert_eq!(mem(e, "RAISE_SALARY").kind, "PROCEDURE");
    }

    #[test]
    fn private_members_are_found_in_the_body() {
        let a = analyze(&inputs());
        let e = pkg(&a, "EMP_PKG");
        let h = mem(e, "HELPER");
        assert!(!h.public);
        assert_eq!(h.status, "unused_private");
        assert!(!mem(e, "LOG_IT").public);
        assert_eq!(mem(e, "LOG_IT").internal_refs, 1);
        assert_eq!(mem(e, "LOG_IT").status, "used");
    }

    #[test]
    fn reads_and_writes_are_classified_and_comments_ignored() {
        let a = analyze(&inputs());
        let e = pkg(&a, "EMP_PKG");
        assert_eq!(mem(e, "RAISE_SALARY").writes, vec!["EMPLOYEES"]);
        assert_eq!(mem(e, "LOG_IT").writes, vec!["AUDIT_LOG"]);
        assert_eq!(mem(e, "HEADCOUNT").reads, vec!["DEPARTMENTS", "EMPLOYEES"]);
        assert!(e.tables.contains(&TableUse {
            table: "EMPLOYEES".into(),
            read: true,
            write: true
        }));
        assert!(!e.tables.iter().any(|t| t.table == "NEVER_SEEN"));
        let u = pkg(&a, "UTIL_PKG");
        assert_eq!(mem(u, "NOTIFY").writes, vec!["AUDIT_LOG"]);
    }

    #[test]
    fn cross_package_calls_including_schema_qualified() {
        let a = analyze(&inputs());
        let e = pkg(&a, "EMP_PKG");
        let u = pkg(&a, "UTIL_PKG");
        assert_eq!(e.calls_packages, vec!["UTIL_PKG"]);
        assert_eq!(u.calls_packages, vec!["EMP_PKG"]);
        assert_eq!(e.called_by, vec!["UTIL_PKG"]);
        assert_eq!(
            mem(e, "RAISE_SALARY").called_by,
            vec![MemberRef {
                package: "UTIL_PKG".into(),
                member: "NOTIFY".into()
            }]
        );
        assert_eq!(mem(u, "NOTIFY").called_by[0].package, "EMP_PKG");
    }

    #[test]
    fn unreferenced_public_members_are_candidates_not_verdicts() {
        let a = analyze(&inputs());
        assert_eq!(mem(pkg(&a, "UTIL_PKG"), "DEAD_ONE").status, "unreferenced");
        assert_eq!(pkg(&a, "UTIL_PKG").unreferenced, 1);
        // HEADCOUNT is public and nothing in the schema calls it.
        assert_eq!(mem(pkg(&a, "EMP_PKG"), "HEADCOUNT").status, "unreferenced");
    }

    #[test]
    fn wrapped_bodies_are_reported_and_never_flagged_unused() {
        let a = analyze(&inputs());
        let w = pkg(&a, "SECRET_PKG");
        assert!(w.wrapped);
        assert_eq!(mem(w, "HIDDEN").status, "used");
        assert_eq!(a.wrapped, 1);
        assert_eq!(a.invalid, 1);
    }

    #[test]
    fn loader_runs_the_five_queries_and_joins_source_lines() {
        let db = FakeDb::new()
            .with_table(
                "pkg:objs",
                FakeTable::new(
                    &["n", "t", "s"],
                    vec![
                        vec![Some("P"), Some("PACKAGE"), Some("VALID")],
                        vec![Some("P"), Some("PACKAGE BODY"), Some("VALID")],
                    ],
                ),
            )
            .with_table(
                "pkg:procs",
                FakeTable::new(&["a", "b", "c"], vec![vec![Some("P"), Some("GO"), None]]),
            )
            .with_table(
                "pkg:args",
                FakeTable::new(&["a", "b", "c", "d", "e", "f", "g"], vec![]),
            )
            .with_table("pkg:tables", FakeTable::new(&["n"], vec![vec![Some("T")]]))
            .with_table(
                "pkg:bodies",
                FakeTable::new(
                    &["n", "t"],
                    vec![
                        vec![Some("P"), Some("PACKAGE BODY p IS\n")],
                        vec![
                            Some("P"),
                            Some("  PROCEDURE go IS BEGIN DELETE FROM t; END;\n"),
                        ],
                        vec![Some("P"), Some("END p;\n")],
                    ],
                ),
            );
        let a = analyze_schema(&db, "HR").unwrap();
        let p = &a.packages[0];
        assert_eq!(p.members[0].writes, vec!["T"]);
        assert_eq!(p.members[0].signature, "GO");
        assert_eq!(p.body_lines, 3);
    }

    /// 400 packages x 40 members x ~15 lines, as in a large schema. Run with
    /// `cargo test -p ora-core --release scale -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn scale_400_packages() {
        let mut inp = Inputs {
            owner: "HR".into(),
            ..Default::default()
        };
        inp.tables = (0..300).map(|i| format!("T{i}")).collect();
        for p in 0..400 {
            let name = format!("PKG{p}");
            inp.packages.push(PkgMeta {
                name: name.clone(),
                spec_status: "VALID".into(),
                body_status: Some("VALID".into()),
            });
            let mut body = format!("PACKAGE BODY {name} IS\n");
            for m in 0..40 {
                inp.procs.push(ProcRow {
                    pkg: name.clone(),
                    name: format!("M{m}"),
                    overload: None,
                });
                body.push_str(&format!(
                    "  PROCEDURE m{m} IS BEGIN\n    UPDATE t{} SET a = 1;\n    SELECT x INTO v FROM t{};\n    pkg{}.m{}(1);\n    NULL;\n  END m{m};\n",
                    (p + m) % 300, (p * 7 + m) % 300, (p + 1) % 400, m
                ));
            }
            body.push_str(&format!("END {name};\n"));
            inp.bodies.insert(name, body);
        }
        let t = std::time::Instant::now();
        let a = analyze(&inp);
        println!(
            "analysed {} packages, {} members in {:?}",
            a.packages.len(),
            a.packages.iter().map(|p| p.members.len()).sum::<usize>(),
            t.elapsed()
        );
        assert_eq!(a.packages.len(), 400);
        assert_eq!(a.packages[0].members.len(), 40);
    }
}
